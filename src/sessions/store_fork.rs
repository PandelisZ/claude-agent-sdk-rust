//! `fork_session_via_store` (upstream `_internal/session_mutations.py`).

use crate::error::{ClaudeSDKError, Result};
use crate::session_store::{project_key_for_directory, SessionKey, SessionStoreHandle};
use crate::sessions::fork_core::{
    build_fork_entries, derive_title_from_entries, partition_fork_entries, ForkSessionResult,
};
use crate::sessions::paths::is_valid_uuid;
use std::path::Path;

/// Fork a session into a new branch with fresh UUIDs via a `SessionStore`.
/// The fork transform runs over the loaded entries and the result is appended
/// under a new session id in the same project.
pub async fn fork_session_via_store(
    session_store: &SessionStoreHandle,
    session_id: &str,
    directory: Option<&str>,
    up_to_message_id: Option<&str>,
    title: Option<&str>,
) -> Result<ForkSessionResult> {
    if !is_valid_uuid(session_id) {
        return Err(ClaudeSDKError::Session(format!(
            "Invalid session_id: {session_id}"
        )));
    }
    if let Some(up_to) = up_to_message_id.filter(|id| !id.is_empty()) {
        if !is_valid_uuid(up_to) {
            return Err(ClaudeSDKError::Session(format!(
                "Invalid up_to_message_id: {up_to}"
            )));
        }
    }

    let project_key = project_key_for_directory(directory.map(Path::new));
    let loaded = session_store
        .load(SessionKey {
            project_key: project_key.clone(),
            session_id: session_id.to_string(),
            subpath: None,
        })
        .await?
        .filter(|entries| !entries.is_empty())
        .ok_or_else(|| {
            ClaudeSDKError::IO(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Session {session_id} not found"),
            ))
        })?;

    let (transcript, replacements) = partition_fork_entries(&loaded, session_id);
    let (forked_session_id, entries) = build_fork_entries(
        transcript,
        replacements,
        session_id,
        up_to_message_id,
        title,
        || derive_title_from_entries(&loaded),
    )?;

    session_store
        .append(
            SessionKey {
                project_key,
                session_id: forked_session_id.clone(),
                subpath: None,
            },
            entries,
        )
        .await?;

    Ok(ForkSessionResult {
        session_id: forked_session_id,
    })
}
