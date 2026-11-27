//! Local-disk `fork_session` (upstream `_internal/session_mutations.py`).

use crate::error::{ClaudeSDKError, Result};
use crate::internal::sessions_fs::find_session_file_with_dir;
use crate::sessions::fork_core::{
    build_fork_entries, derive_title_from_bytes, parse_jsonl_objects, partition_fork_entries,
    ForkSessionResult,
};
use crate::sessions::paths::is_valid_uuid;
use crate::sessions::py_json;

/// Fork a session into a new branch with fresh UUIDs. The fork is written next
/// to the source transcript as `<new id>.jsonl` (mode 0600, never
/// overwriting).
pub async fn fork_session(
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

    let Some((file_path, project_dir)) = find_session_file_with_dir(session_id, directory).await
    else {
        let suffix = directory
            .filter(|dir| !dir.is_empty())
            .map(|dir| format!(" in project directory for {dir}"))
            .unwrap_or_default();
        return Err(ClaudeSDKError::IO(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Session {session_id} not found{suffix}"),
        )));
    };

    let content = tokio::fs::read(&file_path).await?;
    if content.is_empty() {
        return Err(ClaudeSDKError::Session(format!(
            "Session {session_id} has no messages to fork"
        )));
    }
    let (transcript, replacements) =
        partition_fork_entries(&parse_jsonl_objects(&content), session_id);
    let (forked_session_id, entries) = build_fork_entries(
        transcript,
        replacements,
        session_id,
        up_to_message_id,
        title,
        || derive_title_from_bytes(&content),
    )?;

    let mut data = entries
        .iter()
        .map(py_json::dumps_compact)
        .collect::<Vec<_>>()
        .join("\n");
    data.push('\n');
    write_new_file(
        &project_dir.join(format!("{forked_session_id}.jsonl")),
        data.as_bytes(),
    )?;

    Ok(ForkSessionResult {
        session_id: forked_session_id,
    })
}

/// `os.open(path, O_WRONLY | O_CREAT | O_EXCL, 0o600)` + write.
fn write_new_file(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    Ok(())
}
