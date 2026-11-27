//! Materialize a `SessionStore`-backed resume into a temp `CLAUDE_CONFIG_DIR`
//! (upstream `_internal/session_resume.py`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{ClaudeSDKError, Result};
use crate::session_store::{
    project_key_for_directory, SessionKey, SessionListSubkeysKey, SessionStoreEntry,
    SessionStoreHandle,
};
use crate::sessions::chain::{agent_metadata_sidecar_path, split_agent_metadata};
use crate::sessions::paths::{is_valid_uuid, realpath};
use crate::sessions::py_json;
use crate::types::ClaudeAgentOptions;

/// Default macOS Keychain service name for OAuth credentials when
/// `CLAUDE_CONFIG_DIR` is unset.
const KEYCHAIN_SERVICE_NAME: &str = "Claude Code-credentials";

/// User-settings keys that misbehave under a redirected `CLAUDE_CONFIG_DIR`.
const RESUME_SETTINGS_STRIPPED_KEYS: [&str; 2] = ["enabledPlugins", "extraKnownMarketplaces"];

#[derive(Debug, Clone)]
pub struct MaterializedResume {
    pub config_dir: PathBuf,
    pub resume_session_id: String,
}

impl MaterializedResume {
    pub async fn cleanup(&self) {
        let _ = tokio::fs::remove_dir_all(&self.config_dir).await;
    }

    /// [`Self::cleanup`] for callers without a Tokio runtime (a `Drop`).
    pub fn cleanup_blocking(&self) {
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

/// Removes a half-built resume directory (it may already hold
/// `.credentials.json`) unless disarmed, including when the future building
/// it is dropped mid-flight (upstream catches `BaseException`, so
/// cancellation cleans up too).
struct TempDirGuard {
    path: Option<PathBuf>,
}

impl TempDirGuard {
    fn path(&self) -> &Path {
        self.path.as_deref().expect("guard is armed")
    }

    fn disarm(mut self) -> PathBuf {
        self.path.take().expect("guard is armed")
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub async fn materialize_resume_session(
    options: &ClaudeAgentOptions,
) -> Result<Option<MaterializedResume>> {
    let Some(store) = options.session_store.clone() else {
        return Ok(None);
    };
    if options.resume.is_none() && !options.continue_conversation {
        return Ok(None);
    }

    let project_key = project_key_for_directory(options.cwd.as_deref().map(Path::new));
    let timeout = Duration::from_millis(options.load_timeout_ms.max(0) as u64);
    let resolved = if let Some(session_id) = options.resume.as_ref() {
        if !is_valid_uuid(session_id) {
            return Ok(None);
        }
        load_candidate(&store, &project_key, session_id, timeout).await?
    } else {
        resolve_continue_candidate(&store, &project_key, timeout).await?
    };

    let Some((session_id, entries)) = resolved else {
        return Ok(None);
    };

    // From here until it is handed to the caller, the directory is removed
    // on any exit: an error, a panic, or the future being dropped.
    let guard = TempDirGuard {
        path: Some(make_private_temp_dir()?),
    };
    let config_dir = guard.path();
    let project_dir = config_dir.join("projects").join(&project_key);
    tokio::fs::create_dir_all(&project_dir).await?;
    write_jsonl(&project_dir.join(format!("{session_id}.jsonl")), &entries)?;

    copy_auth_files(config_dir, &options.env).await;

    if store.supports_list_subkeys() {
        materialize_subkeys(&store, &project_dir, &project_key, &session_id, timeout).await?;
    }

    Ok(Some(MaterializedResume {
        config_dir: guard.disarm(),
        resume_session_id: session_id,
    }))
}

pub fn apply_materialized_options(
    options: &ClaudeAgentOptions,
    materialized: &MaterializedResume,
) -> ClaudeAgentOptions {
    let mut options = options.clone();
    options.env.insert(
        "CLAUDE_CONFIG_DIR".to_string(),
        materialized.config_dir.to_string_lossy().to_string(),
    );
    options.resume = Some(materialized.resume_session_id.clone());
    options.continue_conversation = false;
    options
}

/// `tempfile.mkdtemp(prefix="claude-resume-")`: a fresh directory readable
/// only by the current user (mode 0700).
fn make_private_temp_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    let mut last_error = None;
    for _ in 0..16 {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let candidate = base.join(format!("claude-resume-{}", &suffix[..12]));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_error = Some(error);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(last_error
        .unwrap_or_else(|| std::io::Error::other("could not create resume temp dir"))
        .into())
}

async fn load_candidate(
    store: &SessionStoreHandle,
    project_key: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<Option<(String, Vec<SessionStoreEntry>)>> {
    let key = SessionKey {
        project_key: project_key.to_string(),
        session_id: session_id.to_string(),
        subpath: None,
    };
    let entries = with_timeout(
        store.load(key),
        timeout,
        format!("SessionStore.load() for session {session_id}"),
    )
    .await?;
    Ok(entries
        .filter(|entries| !entries.is_empty())
        .map(|entries| (session_id.to_string(), entries)))
}

async fn resolve_continue_candidate(
    store: &SessionStoreHandle,
    project_key: &str,
    timeout: Duration,
) -> Result<Option<(String, Vec<SessionStoreEntry>)>> {
    let mut sessions = with_timeout(
        store.list_sessions(project_key),
        timeout,
        "SessionStore.list_sessions()".to_string(),
    )
    .await?;
    sessions.sort_by_key(|session| std::cmp::Reverse(session.mtime));

    for session in sessions {
        if !is_valid_uuid(&session.session_id) {
            continue;
        }
        let Some((session_id, entries)) =
            load_candidate(store, project_key, &session.session_id, timeout).await?
        else {
            continue;
        };
        if entries
            .first()
            .and_then(|entry| entry.get("isSidechain"))
            .and_then(|value| value.as_bool())
            == Some(true)
        {
            continue;
        }
        return Ok(Some((session_id, entries)));
    }

    Ok(None)
}

async fn materialize_subkeys(
    store: &SessionStoreHandle,
    project_dir: &Path,
    project_key: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<()> {
    let session_dir = project_dir.join(session_id);
    let subkeys = with_timeout(
        store.list_subkeys(SessionListSubkeysKey {
            project_key: project_key.to_string(),
            session_id: session_id.to_string(),
        }),
        timeout,
        format!("SessionStore.list_subkeys() for session {session_id}"),
    )
    .await?;

    for subpath in subkeys {
        if !is_safe_subpath(&subpath, &session_dir) {
            tracing::warn!("[SessionStore] skipping unsafe subpath from list_subkeys: {subpath:?}");
            continue;
        }
        let key = SessionKey {
            project_key: project_key.to_string(),
            session_id: session_id.to_string(),
            subpath: Some(subpath.clone()),
        };
        let Some(entries) = with_timeout(
            store.load(key),
            timeout,
            format!("SessionStore.load() for session {session_id} subpath {subpath}"),
        )
        .await?
        .filter(|entries| !entries.is_empty()) else {
            continue;
        };

        let (metadata, transcript) = split_agent_metadata(entries);
        let sub_file = subpath_jsonl(&session_dir, &subpath);
        if !transcript.is_empty() {
            write_jsonl(&sub_file, &transcript)?;
        }
        if let Some(mut metadata) = metadata {
            metadata.shift_remove("type");
            let meta_file = agent_metadata_sidecar_path(&sub_file);
            if let Some(parent) = meta_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            write_private(&meta_file, py_json::dumps_default(&metadata).as_bytes())?;
        }
    }
    Ok(())
}

/// `<session_dir>/<subpath>` with `.jsonl` appended to the final component
/// (never replacing an existing extension).
fn subpath_jsonl(session_dir: &Path, subpath: &str) -> PathBuf {
    let target = session_dir.join(subpath);
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!("{name}.jsonl"))
}

/// Write `data` to `path` with mode 0600 (also tightening an existing file).
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    file.write_all(data)
}

/// `_write_jsonl`: one compact JSON line per entry, mode 0600.
fn write_jsonl(path: &Path, entries: &[SessionStoreEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = String::new();
    for entry in entries {
        out.push_str(&py_json::dumps_compact(entry));
        out.push('\n');
    }
    write_private(path, out.as_bytes())?;
    Ok(())
}

async fn with_timeout<F, T>(future: F, timeout: Duration, label: String) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(result) => result.map_err(|error| {
            ClaudeSDKError::Session(format!(
                "{label} failed during resume materialization: {error}"
            ))
        }),
        Err(_) => Err(ClaudeSDKError::Session(format!(
            "{label} timed out after {}ms during resume materialization",
            timeout.as_millis()
        ))),
    }
}

/// `opt_env.get(name) or os.environ.get(name)`: an empty option value falls
/// through to the process environment (whose value is returned as-is).
fn env_lookup(env: &HashMap<String, String>, name: &str) -> Option<String> {
    env.get(name)
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned()))
}

fn env_truthy(env: &HashMap<String, String>, name: &str) -> bool {
    env_lookup(env, name).is_some_and(|value| !value.is_empty())
}

/// `_copy_auth_files`: seed the temp config dir with `.credentials.json`
/// (refreshToken redacted; from the macOS Keychain when applicable),
/// `.claude.json`, and user `settings.json` / `cowork_settings.json` with
/// plugin declarations stripped.
pub(crate) async fn copy_auth_files(config_dir: &Path, env: &HashMap<String, String>) {
    let caller_config_dir = env_lookup(env, "CLAUDE_CONFIG_DIR");
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let source_config_dir = caller_config_dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));

    let mut creds_json = read_if_present(&source_config_dir.join(".credentials.json"))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());

    if caller_config_dir.is_none()
        && !env_truthy(env, "ANTHROPIC_API_KEY")
        && !env_truthy(env, "CLAUDE_CODE_OAUTH_TOKEN")
    {
        if let Some(keychain) = read_keychain_credentials().await {
            creds_json = Some(keychain);
        }
    }
    write_redacted_credentials(creds_json.as_deref(), &config_dir.join(".credentials.json"));

    let claude_json_src = match caller_config_dir.as_deref().filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => home.join(".claude.json"),
    };
    copy_if_present(&claude_json_src, &config_dir.join(".claude.json"), None);

    for name in ["settings.json", "cowork_settings.json"] {
        copy_if_present(
            &source_config_dir.join(name),
            &config_dir.join(name),
            Some(strip_settings_for_resume),
        );
    }
}

/// `_strip_settings_for_resume`: drop `enabledPlugins`,
/// `extraKnownMarketplaces` and `env.CLAUDE_CONFIG_DIR`. Content that is not a
/// JSON object (or needs no change) is returned untouched.
pub(crate) fn strip_settings_for_resume(content: &[u8]) -> Vec<u8> {
    let body = content.strip_prefix(b"\xef\xbb\xbf").unwrap_or(content);
    let Ok(text) = std::str::from_utf8(body) else {
        return content.to_vec();
    };
    let Ok(serde_json::Value::Object(mut parsed)) = serde_json::from_str::<serde_json::Value>(text)
    else {
        return content.to_vec();
    };
    let mut stripped = false;
    for key in RESUME_SETTINGS_STRIPPED_KEYS {
        if parsed.shift_remove(key).is_some() {
            stripped = true;
        }
    }
    if let Some(serde_json::Value::Object(env_block)) = parsed.get_mut("env") {
        if env_block.shift_remove("CLAUDE_CONFIG_DIR").is_some() {
            stripped = true;
        }
    }
    if !stripped {
        return content.to_vec();
    }
    py_json::dumps_compact(&parsed).into_bytes()
}

/// `_write_redacted_credentials`: rewrite only when
/// `claudeAiOauth.refreshToken` was present; otherwise write the original.
fn write_redacted_credentials(creds_json: Option<&str>, dst: &Path) {
    let Some(creds_json) = creds_json else {
        return;
    };
    let out = redact_refresh_token(creds_json).unwrap_or_else(|| creds_json.to_string());
    if let Err(error) = write_private(dst, out.as_bytes()) {
        tracing::warn!(
            "[SessionStore] resume: could not write {}: {error}",
            dst.display()
        );
    }
}

pub(crate) fn redact_refresh_token(content: &str) -> Option<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let oauth = value.get_mut("claudeAiOauth")?.as_object_mut()?;
    oauth.shift_remove("refreshToken")?;
    Some(py_json::dumps_default(&value))
}

/// `_read_if_present`: read a regular file. Missing is skipped silently;
/// anything else unreadable (EACCES, a directory or FIFO, ...) is logged and
/// skipped.
fn read_if_present(src: &Path) -> Option<Vec<u8>> {
    let result = std::fs::metadata(src).and_then(|meta| {
        if meta.is_file() {
            std::fs::read(src)
        } else {
            Err(std::io::Error::other(format!(
                "{} is not a regular file",
                src.display()
            )))
        }
    });
    match result {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(
                "[SessionStore] resume: skipping {} ({error})",
                src.display()
            );
            None
        }
    }
}

/// A content transform applied while copying a file.
type Transform = fn(&[u8]) -> Vec<u8>;

/// `_copy_if_present`: copy through an optional transform (mode 0600); a
/// partially written destination is removed on failure.
fn copy_if_present(src: &Path, dst: &Path, transform: Option<Transform>) {
    let Some(content) = read_if_present(src) else {
        return;
    };
    let data = match transform {
        Some(transform) => transform(&content),
        None => content,
    };
    if let Err(error) = write_private(dst, &data) {
        let _ = std::fs::remove_file(dst);
        tracing::warn!(
            "[SessionStore] resume: skipping {} ({error})",
            src.display()
        );
    }
}

/// `_read_keychain_credentials`: macOS only; best-effort.
async fn read_keychain_credentials() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let user = ["USER", "LOGNAME", "LNAME", "USERNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "claude-code-user".to_string());
    let child = tokio::process::Command::new("security")
        .args([
            "find-generic-password",
            "-a",
            &user,
            "-w",
            "-s",
            KEYCHAIN_SERVICE_NAME,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!out.is_empty()).then_some(out)
}

/// `_is_safe_subpath`: reject subpaths that are empty, absolute,
/// drive-prefixed, contain `.`/`..` segments (either separator) or NUL, or
/// whose resolved `<target>.jsonl` escapes `session_dir`.
pub(crate) fn is_safe_subpath(subpath: &str, session_dir: &Path) -> bool {
    if subpath.is_empty()
        || Path::new(subpath).is_absolute()
        || subpath.starts_with('/')
        || subpath.starts_with('\\')
    {
        return false;
    }
    // ntpath.splitdrive: `X:` drive prefixes.
    if subpath.chars().nth(1) == Some(':') {
        return false;
    }
    if subpath
        .split(['/', '\\'])
        .any(|segment| segment == "." || segment == "..")
    {
        return false;
    }
    if subpath.contains('\0') {
        return false;
    }
    let target = realpath(&subpath_jsonl(session_dir, subpath));
    target.starts_with(realpath(session_dir))
}
