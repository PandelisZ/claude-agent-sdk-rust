use std::path::{Path, PathBuf};

use crate::error::{CLIConnectionError, CLINotFoundError, ClaudeSDKError, Result};

const DEFAULT_CLI_NAME: &str = "claude";
const MINIMUM_CLAUDE_CODE_VERSION: &str = "2.0.0";
/// First Claude Code version that honors `client_composed` on user messages,
/// which `ClaudeAgentOptions::verbatim_prompts` relies on.
const VERBATIM_PROMPTS_MINIMUM_CLAUDE_CODE_VERSION: (u64, u64, u64) = (2, 1, 248);
const SKIP_VERSION_CHECK_ENV: &str = "CLAUDE_AGENT_SDK_SKIP_VERSION_CHECK";

const UNIX_NOT_FOUND_MESSAGE: &str = "Claude Code not found. Install with:\n  npm install -g @anthropic-ai/claude-code\n\nIf already installed locally, try:\n  export PATH=\"$HOME/node_modules/.bin:$PATH\"\n\nOr provide the path via ClaudeAgentOptions:\n  ClaudeAgentOptions::builder().cli_path(\"/path/to/claude\")";

const WINDOWS_NOT_FOUND_MESSAGE: &str = "Claude Code not found. Install the native claude.exe with (PowerShell):\n  irm https://claude.ai/install.ps1 | iex\n\nOr provide the path to a claude.exe via ClaudeAgentOptions:\n  ClaudeAgentOptions::builder().cli_path(\"C:\\\\path\\\\to\\\\claude.exe\")\n\n(npm install -g @anthropic-ai/claude-code produces a claude.cmd shim, which this SDK refuses to run on Windows.)";

pub(crate) fn find_cli_path(explicit: Option<&str>) -> Result<String> {
    if let Some(path) = explicit {
        return Ok(path.to_string());
    }

    if let Some(path) = bundled_cli_path().filter(|path| path.is_file()) {
        return Ok(path.to_string_lossy().to_string());
    }

    let home = dirs::home_dir();
    let locations = if cfg!(windows) {
        // Only a native claude.exe, and only from absolute PATH entries:
        // `which` would resolve npm's claude.cmd shim (refused by connect)
        // through PATHEXT, and a relative entry searches the working directory.
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        if let Some(exe) = windows_cli_from_path(std::env::split_paths(&path_var), |p| p.is_file())
        {
            return Ok(exe.to_string_lossy().to_string());
        }
        home.as_deref()
            .map(windows_fallback_cli_locations)
            .unwrap_or_default()
    } else {
        if let Ok(path) = which::which(DEFAULT_CLI_NAME) {
            return Ok(path.to_string_lossy().to_string());
        }
        home.as_deref()
            .map(fallback_cli_locations)
            .unwrap_or_default()
    };

    for path in locations {
        if path.is_file() {
            return Ok(path.to_string_lossy().to_string());
        }
    }

    Err(ClaudeSDKError::CLINotFound(CLINotFoundError::new(
        cli_not_found_message(cfg!(windows)),
        DEFAULT_CLI_NAME,
    )))
}

pub(crate) fn cli_not_found_message(windows: bool) -> &'static str {
    if windows {
        WINDOWS_NOT_FOUND_MESSAGE
    } else {
        UNIX_NOT_FOUND_MESSAGE
    }
}

/// The first `<dir>/claude.exe` that is a file, over absolute PATH entries.
pub(crate) fn windows_cli_from_path<I, F>(entries: I, is_file: F) -> Option<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
    F: Fn(&Path) -> bool,
{
    entries
        .into_iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("claude.exe"))
        .find(|exe| is_file(exe))
}

pub(crate) fn windows_fallback_cli_locations(home: &Path) -> Vec<PathBuf> {
    vec![home.join(".local/bin/claude.exe")]
}

pub(crate) fn fallback_cli_locations(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".npm-global/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
        home.join(".local/bin/claude"),
        home.join("node_modules/.bin/claude"),
        home.join(".yarn/bin/claude"),
        home.join(".claude/local/claude"),
    ]
}

/// Whether `cli_path` names a .bat/.cmd batch script, judged by Win32 path
/// rules: any component (split also on `:` for drive prefixes and NTFS stream
/// specs), with trailing dots and spaces stripped, ending in `.bat`/`.cmd`.
/// Platform-independent; callers gate it on `cfg!(windows)`.
pub(crate) fn is_windows_batch_path(cli_path: &str) -> bool {
    cli_path
        .replace('\\', "/")
        .split('/')
        .flat_map(|component| component.split(':'))
        .any(|segment| {
            let segment = segment.trim_end_matches(['.', ' ']).to_lowercase();
            segment.ends_with(".bat") || segment.ends_with(".cmd")
        })
}

/// Refuse to execute a .bat/.cmd script as the CLI on Windows: cmd.exe
/// re-parses the command line, and no reliable escaping for it exists, so
/// metacharacters in argument values could execute injected commands.
pub(crate) fn reject_windows_batch_cli(cli_path: &str, windows: bool) -> Result<()> {
    if !windows || !is_windows_batch_path(cli_path) {
        return Ok(());
    }
    Err(CLIConnectionError::new(format!(
        "Refusing to execute batch script {}: Windows runs .bat/.cmd files via cmd.exe, which can execute commands injected through CLI arguments, and no reliable escaping for cmd.exe exists. Use a native claude executable instead: install Claude Code natively (irm https://claude.ai/install.ps1 | iex), or point ClaudeAgentOptions::cli_path at a claude.exe.",
        crate::internal::cli_args::py_repr(cli_path)
    ))
    .into())
}

/// Python `re.match(r"(\d{1,9})\.(\d{1,9})\.(\d{1,9})", version)`.
pub(crate) fn parse_version_prefix(version: &str) -> Option<(u64, u64, u64)> {
    fn digits(s: &str, then_dot: bool) -> Option<(u64, &str)> {
        let len = s.bytes().take_while(u8::is_ascii_digit).count();
        if len == 0 || (then_dot && len > 9) {
            return None;
        }
        let len = len.min(9);
        let rest = &s[len..];
        let rest = if then_dot {
            rest.strip_prefix('.')?
        } else {
            rest
        };
        Some((s[..len].parse().ok()?, rest))
    }
    let (major, rest) = digits(version, true)?;
    let (minor, rest) = digits(rest, true)?;
    let (patch, _) = digits(rest, false)?;
    Some((major, minor, patch))
}

/// Whether `CLAUDE_AGENT_SDK_SKIP_VERSION_CHECK` is set to a non-empty value.
pub(crate) fn version_check_skipped() -> bool {
    std::env::var_os(SKIP_VERSION_CHECK_ENV).is_some_and(|value| !value.is_empty())
}

/// The warnings the Python SDK logs for a `system`/`init` message's
/// `claude_code_version`. Returns `None` when `message` is not an init
/// message.
pub(crate) fn claude_version_warnings(
    cli_path: &str,
    message: &serde_json::Value,
    verbatim_prompts: bool,
) -> Option<Vec<String>> {
    if message.get("type").and_then(|v| v.as_str()) != Some("system")
        || message.get("subtype").and_then(|v| v.as_str()) != Some("init")
    {
        return None;
    }
    let version = match message.get("claude_code_version") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    };
    let parsed = version.as_deref().and_then(parse_version_prefix);
    let mut warnings = Vec::new();
    if parsed.is_none() {
        warnings.push(format!(
            "Claude Code at {cli_path} is too old to report its version, and may be unsupported in the Agent SDK. Minimum required version is {MINIMUM_CLAUDE_CODE_VERSION}. Some features may not work correctly."
        ));
    }
    if verbatim_prompts && parsed.is_none_or(|v| v < VERBATIM_PROMPTS_MINIMUM_CLAUDE_CODE_VERSION) {
        let (a, b, c) = VERBATIM_PROMPTS_MINIMUM_CLAUDE_CODE_VERSION;
        warnings.push(format!(
            "verbatim_prompts is enabled, but Claude Code version {} at {cli_path} ignores it: prompts will still have @path mentions expanded and slash commands dispatched. Claude Code {a}.{b}.{c} or later is required.",
            version.as_deref().filter(|v| !v.is_empty()).unwrap_or("unknown"),
        ));
    }
    Some(warnings)
}

/// Once-per-session CLI version check, fed from the first `system`/`init`
/// message on stdout instead of a separate `cli -v` run.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Wired in by the client/query read loop.
pub(crate) struct ClaudeVersionCheck {
    checked: bool,
    cli_path: String,
    verbatim_prompts: bool,
}

#[allow(dead_code)] // Wired in by the client/query read loop.
impl ClaudeVersionCheck {
    pub(crate) fn new(cli_path: impl Into<String>, verbatim_prompts: bool) -> Self {
        Self {
            checked: version_check_skipped(),
            cli_path: cli_path.into(),
            verbatim_prompts,
        }
    }

    /// Build from options, resolving the CLI path the transport would use.
    pub(crate) fn from_options(options: &crate::types::ClaudeAgentOptions) -> Self {
        let cli_path = find_cli_path(options.cli_path.as_deref())
            .unwrap_or_else(|_| DEFAULT_CLI_NAME.to_string());
        Self::new(cli_path, options.verbatim_prompts)
    }

    /// Inspect one stdout message; logs warnings the first time an init
    /// message arrives. Returns the warnings emitted by this call.
    pub(crate) fn observe(&mut self, message: &serde_json::Value) -> Vec<String> {
        if self.checked {
            return Vec::new();
        }
        let Some(warnings) =
            claude_version_warnings(&self.cli_path, message, self.verbatim_prompts)
        else {
            return Vec::new();
        };
        self.checked = true;
        for warning in &warnings {
            tracing::warn!("{}", warning);
        }
        warnings
    }
}

fn bundled_cli_path() -> Option<PathBuf> {
    let cli_name = if cfg!(windows) {
        "claude.exe"
    } else {
        DEFAULT_CLI_NAME
    };
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join("_bundled").join(cli_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_locations_match_python_sdk_order() {
        let home = PathBuf::from("/home/alice");
        let locations = fallback_cli_locations(&home);

        assert_eq!(
            locations[0],
            PathBuf::from("/home/alice/.npm-global/bin/claude")
        );
        assert_eq!(locations[1], PathBuf::from("/usr/local/bin/claude"));
        assert_eq!(locations[2], PathBuf::from("/home/alice/.local/bin/claude"));
        assert_eq!(
            locations[3],
            PathBuf::from("/home/alice/node_modules/.bin/claude")
        );
        assert_eq!(locations[4], PathBuf::from("/home/alice/.yarn/bin/claude"));
        assert_eq!(
            locations[5],
            PathBuf::from("/home/alice/.claude/local/claude")
        );
    }

    #[test]
    fn windows_discovery_uses_absolute_path_entries_and_claude_exe_only() {
        let entries = vec![
            PathBuf::from("relative/bin"),
            PathBuf::from("/abs/one"),
            PathBuf::from("/abs/two"),
        ];
        let found = windows_cli_from_path(entries.clone(), |p| {
            p == Path::new("/abs/two/claude.exe") || p == Path::new("relative/bin/claude.exe")
        });
        assert_eq!(found, Some(PathBuf::from("/abs/two/claude.exe")));
        assert_eq!(
            windows_cli_from_path(entries, |p| p == Path::new("relative/bin/claude.exe")),
            None
        );
        assert_eq!(
            windows_fallback_cli_locations(Path::new("/home/alice")),
            vec![PathBuf::from("/home/alice/.local/bin/claude.exe")]
        );
    }

    #[test]
    fn not_found_messages_match_python_sdk() {
        let unix = cli_not_found_message(false);
        assert!(unix.starts_with("Claude Code not found. Install with:\n  npm install -g @anthropic-ai/claude-code\n\nIf already installed locally, try:\n  export PATH=\"$HOME/node_modules/.bin:$PATH\"\n\nOr provide the path via ClaudeAgentOptions:\n"));
        let windows = cli_not_found_message(true);
        assert!(windows.contains("irm https://claude.ai/install.ps1 | iex"));
        assert!(windows.contains("claude.cmd shim"));
    }

    #[test]
    fn batch_cli_detection_matches_python_rules() {
        for path in [
            "claude.cmd",
            "C:\\npm\\claude.CMD",
            "C:/npm/claude.bat",
            "claude.cmd.",
            "claude.cmd .. ",
            ".cmd",
            "C:claude.cmd",
            "claude.cmd:stream",
            "claude:evil.cmd",
            "C:\\x.bat\\claude.exe",
        ] {
            assert!(is_windows_batch_path(path), "{path} should be refused");
        }
        for path in ["C:\\bin\\claude.exe", "claude", "/usr/bin/claude", "cmd"] {
            assert!(!is_windows_batch_path(path), "{path} should be allowed");
        }
        assert!(reject_windows_batch_cli("claude.cmd", false).is_ok());
        let err = reject_windows_batch_cli("claude.cmd", true).unwrap_err();
        assert!(err
            .to_string()
            .contains("Refusing to execute batch script 'claude.cmd'"));
    }

    #[test]
    fn parses_version_like_python_regex_match() {
        assert_eq!(parse_version_prefix("2.1.110"), Some((2, 1, 110)));
        assert_eq!(parse_version_prefix("2.0.0 (Claude Code)"), Some((2, 0, 0)));
        assert_eq!(
            parse_version_prefix("2.1.1234567890"),
            Some((2, 1, 123456789))
        );
        assert_eq!(parse_version_prefix("1234567890.1.1"), None);
        assert_eq!(parse_version_prefix("v2.1.0"), None);
        assert_eq!(parse_version_prefix("2.1"), None);
        assert_eq!(parse_version_prefix("not-a-version"), None);
    }

    #[test]
    fn version_warnings_from_init_message() {
        let init = |v: serde_json::Value| serde_json::json!({"type": "system", "subtype": "init", "claude_code_version": v});
        assert_eq!(
            claude_version_warnings("/c", &serde_json::json!({"type": "assistant"}), true),
            None
        );
        assert_eq!(
            claude_version_warnings("/c", &init("2.1.248".into()), true),
            Some(vec![])
        );
        assert_eq!(
            claude_version_warnings("/c", &init("2.0.0".into()), false),
            Some(vec![])
        );

        let old = claude_version_warnings("/c", &init("2.1.247".into()), true).unwrap();
        assert_eq!(old, vec!["verbatim_prompts is enabled, but Claude Code version 2.1.247 at /c ignores it: prompts will still have @path mentions expanded and slash commands dispatched. Claude Code 2.1.248 or later is required.".to_string()]);

        let missing = claude_version_warnings(
            "/c",
            &serde_json::json!({"type": "system", "subtype": "init"}),
            true,
        )
        .unwrap();
        assert_eq!(missing, vec![
            "Claude Code at /c is too old to report its version, and may be unsupported in the Agent SDK. Minimum required version is 2.0.0. Some features may not work correctly.".to_string(),
            "verbatim_prompts is enabled, but Claude Code version unknown at /c ignores it: prompts will still have @path mentions expanded and slash commands dispatched. Claude Code 2.1.248 or later is required.".to_string(),
        ]);
    }

    #[test]
    fn version_check_runs_once() {
        let mut check = ClaudeVersionCheck {
            checked: false,
            cli_path: "/c".to_string(),
            verbatim_prompts: false,
        };
        let init = serde_json::json!({"type": "system", "subtype": "init"});
        assert!(check
            .observe(&serde_json::json!({"type": "system", "subtype": "status"}))
            .is_empty());
        assert_eq!(check.observe(&init).len(), 1);
        assert!(check.observe(&init).is_empty());
    }
}
