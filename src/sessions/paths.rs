//! Config/project directory resolution shared by every local-disk session
//! helper. Ports `_get_claude_config_home_dir`, `_get_projects_dir`,
//! `_canonicalize_path`, `_sanitize_path`, `_find_project_dir` and
//! `_get_worktree_paths` from upstream `_internal/sessions.py`.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use unicode_normalization::UnicodeNormalization;

/// Maximum length of a sanitized project directory name before hashing.
pub(crate) const MAX_SANITIZED_LENGTH: usize = 200;

/// Unicode NFC normalization (`unicodedata.normalize("NFC", s)`).
pub(crate) fn nfc(value: &str) -> String {
    value.nfc().collect()
}

/// Strict hyphenated UUID check (`_UUID_RE`), case-insensitive.
pub(crate) fn is_valid_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(idx, byte)| match idx {
        8 | 13 | 18 | 23 => *byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    })
}

/// A process environment variable, treating an empty value as unset.
pub(crate) fn env_nonempty(name: &str) -> Option<String> {
    std::env::var_os(name)
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.claude`, or `CLAUDE_CONFIG_DIR` when set to a non-empty value (NFC).
pub(crate) fn claude_config_home_dir() -> PathBuf {
    match env_nonempty("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(nfc(&dir)),
        None => PathBuf::from(nfc(&home_dir().join(".claude").to_string_lossy())),
    }
}

/// `<config dir>/projects`. `env_override` (e.g. `options.env`) wins over the
/// process environment so callers resolve the directory the subprocess uses.
pub(crate) fn projects_dir(env_override: Option<&HashMap<String, String>>) -> PathBuf {
    if let Some(dir) = env_override
        .and_then(|env| env.get("CLAUDE_CONFIG_DIR"))
        .filter(|dir| !dir.is_empty())
    {
        return PathBuf::from(nfc(dir)).join("projects");
    }
    claude_config_home_dir().join("projects")
}

/// Non-strict `os.path.realpath`: makes the path absolute, resolves symlinks
/// for the longest existing prefix and normalizes the remainder lexically.
pub(crate) fn realpath(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    if let Ok(resolved) = std::fs::canonicalize(&absolute) {
        return resolved;
    }
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(part) => {
                resolved.push(part);
                if let Ok(canonical) = std::fs::canonicalize(&resolved) {
                    resolved = canonical;
                }
            }
        }
    }
    resolved
}

/// `_canonicalize_path`: realpath + NFC.
pub(crate) fn canonicalize_path(directory: &str) -> String {
    nfc(&realpath(Path::new(directory)).to_string_lossy())
}

/// 32-bit JS-style string hash rendered in base 36 (`_simple_hash`).
pub(crate) fn simple_hash(value: &str) -> String {
    let mut hash = 0i32;
    for ch in value.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(ch as i32);
    }
    let mut n = hash.unsigned_abs();
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        let digit = (n % 36) as u8;
        out.push(match digit {
            0..=9 => (b'0' + digit) as char,
            _ => (b'a' + digit - 10) as char,
        });
        n /= 36;
    }
    out.iter().rev().collect()
}

/// `_sanitize_path`: every non-ASCII-alphanumeric code point becomes `-`;
/// long names are truncated and suffixed with a hash.
pub(crate) fn sanitize_path(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    if sanitized.len() <= MAX_SANITIZED_LENGTH {
        sanitized
    } else {
        format!(
            "{}-{}",
            &sanitized[..MAX_SANITIZED_LENGTH],
            simple_hash(name)
        )
    }
}

/// `_find_project_dir`: exact sanitized match, else (for names longer than
/// [`MAX_SANITIZED_LENGTH`]) the first directory sharing the truncated prefix.
pub(crate) fn find_project_dir(project_path: &str) -> Option<PathBuf> {
    let projects = projects_dir(None);
    let sanitized = sanitize_path(project_path);
    let exact = projects.join(&sanitized);
    if exact.is_dir() {
        return Some(exact);
    }
    if sanitized.len() <= MAX_SANITIZED_LENGTH {
        return None;
    }
    let prefix = format!("{}-", &sanitized[..MAX_SANITIZED_LENGTH]);
    std::fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix))
        })
}

/// `_get_worktree_paths`: `git worktree list --porcelain` (5 s timeout) run in
/// `cwd`. Returns an empty list when git is unavailable or `cwd` is not a repo.
pub(crate) async fn get_worktree_paths(cwd: &str) -> Vec<String> {
    let child = tokio::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let Ok(child) = child else {
        return Vec::new();
    };
    let Ok(Ok(output)) =
        tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await
    else {
        return Vec::new();
    };
    if !output.status.success() || output.stdout.is_empty() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .split('\n')
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(nfc)
        .collect()
}

/// Every directory entry under the projects directory (`None` when the
/// projects directory cannot be read).
pub(crate) fn project_dir_entries(only_dirs: bool) -> Option<Vec<PathBuf>> {
    let entries = std::fs::read_dir(projects_dir(None)).ok()?;
    Some(
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| !only_dirs || path.is_dir())
            .collect(),
    )
}

/// Candidate project directories for a session lookup with an explicit
/// `directory`: the canonical project dir first, then each other git
/// worktree's project dir. Git is only consulted when `probe` finds nothing in
/// the primary dir, matching the upstream lookup order. `probe` receives the
/// project dir and the project path it belongs to.
pub(crate) async fn probe_directory_candidates<T, F>(
    directory: &str,
    mut probe: F,
) -> crate::error::Result<Option<T>>
where
    F: FnMut(&Path, &str) -> crate::error::Result<Option<T>>,
{
    let canonical = canonicalize_path(directory);
    if let Some(project_dir) = find_project_dir(&canonical) {
        if let Some(found) = probe(&project_dir, &canonical)? {
            return Ok(Some(found));
        }
    }
    for worktree in get_worktree_paths(&canonical).await {
        if worktree == canonical {
            continue;
        }
        if let Some(project_dir) = find_project_dir(&worktree) {
            if let Some(found) = probe(&project_dir, &worktree)? {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_validation_is_strict_hyphenated() {
        assert!(is_valid_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_valid_uuid("550E8400-E29B-41D4-A716-446655440000"));
        assert!(!is_valid_uuid("550e8400e29b41d4a716446655440000"));
        assert!(!is_valid_uuid("{550e8400-e29b-41d4-a716-446655440000}"));
        assert!(!is_valid_uuid(
            "urn:uuid:550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(!is_valid_uuid("../../etc/passwd"));
    }

    #[test]
    fn sanitizes_like_python() {
        assert_eq!(
            sanitize_path("/Users/alice/my project"),
            "-Users-alice-my-project"
        );
        assert_eq!(sanitize_path("/tmp/caf\u{e9}"), "-tmp-caf-");
        let long = "a".repeat(MAX_SANITIZED_LENGTH + 1);
        assert!(sanitize_path(&long).starts_with(&format!("{}-", "a".repeat(200))));
        assert_eq!(simple_hash("abc"), "22ci");
    }

    #[test]
    fn realpath_normalizes_missing_suffix() {
        let base = realpath(&std::env::temp_dir());
        let missing = base.join("does-not-exist-xyz").join("..").join("other");
        assert_eq!(realpath(&missing), base.join("other"));
    }
}
