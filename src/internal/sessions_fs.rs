//! Session filesystem operations.

use std::path::PathBuf;
use crate::error::Result;
use crate::sessions::{ListSessionsOptions, SessionInfo, SessionMessage};

/// Get the base directory for Claude sessions.
fn get_sessions_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

/// List all sessions from the filesystem.
pub async fn list_sessions(opts: &ListSessionsOptions) -> Result<Vec<SessionInfo>> {
    let sessions_dir = get_sessions_dir();
    let mut sessions = Vec::new();
    
    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            if let Some(session_id) = entry.file_name().to_str() {
                if let Ok(info) = get_session_info(session_id).await {
                    sessions.push(info);
                }
            }
        }
    }
    
    // Apply limit and offset
    if let Some(offset) = opts.offset {
        sessions = sessions.into_iter().skip(offset).collect();
    }
    if let Some(limit) = opts.limit {
        sessions = sessions.into_iter().take(limit).collect();
    }
    
    Ok(sessions)
}

/// Get information about a specific session.
pub async fn get_session_info(session_id: &str) -> Result<SessionInfo> {
    let session_dir = get_sessions_dir().join(session_id);
    let metadata_path = session_dir.join("metadata.json");
    
    if metadata_path.exists() {
        let content = tokio::fs::read_to_string(&metadata_path).await?;
        let info: SessionInfo = serde_json::from_str(&content)?;
        Ok(info)
    } else {
        // Return basic info if metadata does not exist
        Ok(SessionInfo {
            id: session_id.to_string(),
            title: session_id.to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            message_count: 0,
        })
    }
}

/// Get all messages for a specific session.
pub async fn get_session_messages(session_id: &str) -> Result<Vec<SessionMessage>> {
    let session_dir = get_sessions_dir().join(session_id);
    let messages_path = session_dir.join("messages.json");
    
    if messages_path.exists() {
        let content = tokio::fs::read_to_string(&messages_path).await?;
        let messages: Vec<SessionMessage> = serde_json::from_str(&content)?;
        Ok(messages)
    } else {
        Ok(Vec::new())
    }
}

/// Rename a session.
pub async fn rename_session(session_id: &str, title: &str) -> Result<()> {
    let session_dir = get_sessions_dir().join(session_id);
    let metadata_path = session_dir.join("metadata.json");
    
    let mut info = if metadata_path.exists() {
        let content = tokio::fs::read_to_string(&metadata_path).await?;
        serde_json::from_str::<SessionInfo>(&content)?
    } else {
        SessionInfo {
            id: session_id.to_string(),
            title: session_id.to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            message_count: 0,
        }
    };
    
    info.title = title.to_string();
    
    // Ensure directory exists
    tokio::fs::create_dir_all(&session_dir).await?;
    
    let content = serde_json::to_string_pretty(&info)?;
    tokio::fs::write(&metadata_path, content).await?;
    
    Ok(())
}

/// Delete a session.
pub async fn delete_session(session_id: &str) -> Result<()> {
    let session_dir = get_sessions_dir().join(session_id);
    if session_dir.exists() {
        tokio::fs::remove_dir_all(&session_dir).await?;
    }
    Ok(())
}
