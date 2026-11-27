//! App label handling for the Claude Code subprocess (Python
//! `_internal/_client_app.py`).
//!
//! `CLAUDE_AGENT_SDK_CLIENT_APP` carries the developer's explicit
//! `ClaudeAgentOptions::client_app`. `CLAUDE_AGENT_SDK_DERIVED_CLIENT_APP`
//! carries a label the SDK works out itself; upstream gates derivation on a
//! Claude Code version no release meets yet (`999.0.0`), so the Rust SDK never
//! derives one. It still strips an inherited derived label, which names an
//! outer SDK session's app rather than this one.

/// Explicit app label, from `ClaudeAgentOptions::client_app`.
pub const EXPLICIT_CLIENT_APP_ENV: &str = "CLAUDE_AGENT_SDK_CLIENT_APP";
/// Derived app label; always removed from the child environment.
pub const DERIVED_CLIENT_APP_ENV: &str = "CLAUDE_AGENT_SDK_DERIVED_CLIENT_APP";
