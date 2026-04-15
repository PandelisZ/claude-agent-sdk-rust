//! Session management for Claude Agent SDK.

use serde::{Deserialize, Serialize};
use crate::error::Result;
use crate::internal::sessions_fs;

/// Information about a Claude session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
}

/// A message within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

/// Options for listing sessions.
#[derive(Debug, Clone)]
pub struct ListSessionsOptions {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

impl Default for ListSessionsOptions {
    fn default() -> Self {
        Self { limit: None, offset: None }
    }
}

/// Options for querying sessions.
#[derive(Debug, Clone)]
pub struct SessionQueryOptions {
    pub session_id: String,
    pub include_messages: bool,
}

/// Options for mutating sessions.
#[derive(Debug, Clone)]
pub struct SessionMutationOptions {
    pub session_id: String,
}

/// List all available sessions.
pub async fn list_sessions(opts: &ListSessionsOptions) -> Result<Vec<SessionInfo>> {
    sessions_fs::list_sessions(opts).await
}

/// Get information about a specific session.
pub async fn get_session_info(session_id: &str, _opts: &SessionQueryOptions) -> Result<SessionInfo> {
    sessions_fs::get_session_info(session_id).await
}

/// Get all messages for a specific session.
pub async fn get_session_messages(session_id: &str, _opts: &SessionQueryOptions) -> Result<Vec<SessionMessage>> {
    sessions_fs::get_session_messages(session_id).await
}

/// Rename a session.
pub async fn rename_session(session_id: &str, title: &str, _opts: &SessionMutationOptions) -> Result<()> {
    sessions_fs::rename_session(session_id, title).await
}

/// Delete a session.
pub async fn delete_session(session_id: &str, _opts: &SessionMutationOptions) -> Result<()> {
    sessions_fs::delete_session(session_id).await
}
