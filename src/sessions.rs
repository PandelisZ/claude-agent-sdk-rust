//! Session management for Claude Agent SDK.
//!
//! Local-disk helpers (`list_sessions`, `get_session_messages`, ...) read the
//! CLI's `~/.claude/projects/<project>/<session>.jsonl` transcripts; the
//! `*_from_store` / `*_via_store` variants operate on a [`SessionStore`].
//!
//! [`SessionStore`]: crate::session_store::SessionStore

use crate::error::Result;
use crate::internal::sessions_fs;
use serde::{Deserialize, Serialize};

pub(crate) mod chain;
pub(crate) mod fork_core;
pub(crate) mod lite;
pub(crate) mod paths;
pub(crate) mod py_json;
pub(crate) mod sanitize;

pub mod store;
pub use store::*;
pub mod store_fork;
pub use store_fork::*;
pub mod import;
pub use import::*;
pub mod local_fork;
pub use fork_core::ForkSessionResult;

#[cfg(test)]
mod chain_tests;

/// Former name of the local session metadata type; local listings now return
/// the same [`SDKSessionInfo`] shape as Python's `list_sessions()`.
pub type SessionInfo = SDKSessionInfo;

/// Former name of the fork result returned by [`fork_session`].
pub type LocalForkSessionResult = ForkSessionResult;

/// A user or assistant message from a session transcript (Python
/// `SessionMessage`), returned by the local and store-backed readers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessage {
    /// `"user"` or `"assistant"`.
    pub r#type: String,
    /// Unique message identifier (entry `uuid`).
    pub uuid: String,
    /// ID of the session this message belongs to (entry `sessionId`).
    pub session_id: String,
    /// Raw Anthropic API message object (role, content, ...), unmodified.
    pub message: serde_json::Value,
    /// For subagent messages: the Agent `tool_use` id in the parent session
    /// that spawned the subagent. Always `None` for main-session messages.
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    /// For subagent messages: the agent id of the spawning subagent (nested
    /// subagents). Always `None` for main-session messages.
    #[serde(default)]
    pub parent_agent_id: Option<String>,
}

/// Options for listing sessions.
#[derive(Debug, Clone)]
pub struct ListSessionsOptions {
    /// Project directory to list sessions for; `None` lists every project.
    pub directory: Option<String>,
    /// Maximum number of sessions to return (`Some(0)` means no limit).
    pub limit: Option<usize>,
    /// Number of sessions to skip from the start of the sorted result.
    pub offset: Option<usize>,
    /// When `directory` is inside a git repository, also include sessions
    /// from every git worktree of that repository. Defaults to `true`.
    pub include_worktrees: bool,
}

impl Default for ListSessionsOptions {
    fn default() -> Self {
        Self {
            directory: None,
            limit: None,
            offset: None,
            include_worktrees: true,
        }
    }
}

/// Options for querying sessions.
#[derive(Debug, Clone)]
pub struct SessionQueryOptions {
    pub session_id: String,
    pub include_messages: bool,
    pub directory: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// Options for mutating sessions.
#[derive(Debug, Clone)]
pub struct SessionMutationOptions {
    pub session_id: String,
    pub directory: Option<String>,
}

/// List sessions with metadata extracted from stat + head/tail reads, sorted
/// by `last_modified` descending.
pub async fn list_sessions(opts: &ListSessionsOptions) -> Result<Vec<SDKSessionInfo>> {
    Ok(sessions_fs::list_sessions(opts).await)
}

/// Read metadata for a single session. `Ok(None)` when the session id is not
/// a UUID, the file is missing/empty, it is a sidechain session, or it has no
/// extractable summary.
pub async fn get_session_info(
    session_id: &str,
    opts: &SessionQueryOptions,
) -> Result<Option<SDKSessionInfo>> {
    Ok(sessions_fs::get_session_info(session_id, opts.directory.as_deref()).await)
}

/// Read a session's conversation messages (the `parentUuid` chain, with
/// parallel tool calls/results restored). Empty for an invalid or missing
/// session.
pub async fn get_session_messages(
    session_id: &str,
    opts: &SessionQueryOptions,
) -> Result<Vec<SessionMessage>> {
    Ok(sessions_fs::get_session_messages(
        session_id,
        opts.directory.as_deref(),
        opts.limit,
        opts.offset.unwrap_or(0),
    )
    .await)
}

/// List subagent IDs for a specific session. Empty for an invalid or missing
/// session.
pub async fn list_subagents(session_id: &str, opts: &SessionQueryOptions) -> Result<Vec<String>> {
    Ok(sessions_fs::list_subagents(session_id, opts.directory.as_deref()).await)
}

/// Get messages for a specific subagent transcript. Each message carries the
/// `parent_tool_use_id` / `parent_agent_id` recorded in the subagent's
/// `.meta.json` sidecar.
pub async fn get_subagent_messages(
    session_id: &str,
    agent_id: &str,
    opts: &SessionQueryOptions,
) -> Result<Vec<SessionMessage>> {
    Ok(sessions_fs::get_subagent_messages(
        session_id,
        agent_id,
        opts.directory.as_deref(),
        opts.limit,
        opts.offset.unwrap_or(0),
    )
    .await)
}

/// Rename a session by appending a `custom-title` entry.
pub async fn rename_session(
    session_id: &str,
    title: &str,
    opts: &SessionMutationOptions,
) -> Result<()> {
    sessions_fs::rename_session(session_id, title, opts.directory.as_deref()).await
}

/// Tag a session. Pass `None` to clear the tag.
pub async fn tag_session(
    session_id: &str,
    tag: Option<&str>,
    opts: &SessionMutationOptions,
) -> Result<()> {
    sessions_fs::tag_session(session_id, tag, opts.directory.as_deref()).await
}

/// Fork a local session transcript into a new session file.
pub async fn fork_session(
    session_id: &str,
    opts: &SessionMutationOptions,
    up_to_message_id: Option<&str>,
    title: Option<&str>,
) -> Result<ForkSessionResult> {
    local_fork::fork_session(
        session_id,
        opts.directory.as_deref(),
        up_to_message_id,
        title,
    )
    .await
}

/// Delete a session transcript and its subagent directory.
pub async fn delete_session(session_id: &str, opts: &SessionMutationOptions) -> Result<()> {
    sessions_fs::delete_session(session_id, opts.directory.as_deref()).await
}
