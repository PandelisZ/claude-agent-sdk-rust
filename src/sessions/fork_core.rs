//! Fork transform shared by `fork_session` and `fork_session_via_store`.
//! Ports `_build_fork_lines`, `_parse_fork_transcript` and
//! `_derive_title_from_entries` from upstream `_internal/session_mutations.py`.

use crate::error::{ClaudeSDKError, Result};
use crate::sessions::chain::{truthy, Entry};
use crate::sessions::{lite, py_json};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Transcript types copied into a fork (`_TRANSCRIPT_TYPES`).
const FORK_TRANSCRIPT_TYPES: [&str; 5] = ["user", "assistant", "attachment", "system", "progress"];

/// Result of a fork operation (shared by the local and store paths).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForkSessionResult {
    /// UUID of the new forked session.
    pub session_id: String,
}

/// `datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")`:
/// microsecond precision, fraction omitted when it is zero.
pub(crate) fn iso_now() -> String {
    let now = chrono::Utc::now();
    let micros = now.timestamp_subsec_micros();
    if micros == 0 {
        now.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    } else {
        format!("{}.{micros:06}Z", now.format("%Y-%m-%dT%H:%M:%S"))
    }
}

/// Partition raw entries into transcript entries (string `uuid`, transcript
/// type) and the `replacements` of `content-replacement` records for
/// `session_id` (`_parse_fork_transcript` over already-parsed entries).
pub(crate) fn partition_fork_entries(raw: &[Entry], session_id: &str) -> (Vec<Entry>, Vec<Value>) {
    let mut transcript = Vec::new();
    let mut replacements = Vec::new();
    for entry in raw {
        let entry_type = entry.get("type").and_then(Value::as_str);
        if entry_type.is_some_and(|ty| FORK_TRANSCRIPT_TYPES.contains(&ty))
            && entry.get("uuid").is_some_and(Value::is_string)
        {
            transcript.push(entry.clone());
        } else if entry_type == Some("content-replacement")
            && entry.get("sessionId").and_then(Value::as_str) == Some(session_id)
        {
            if let Some(Value::Array(items)) = entry.get("replacements") {
                replacements.extend(items.iter().cloned());
            }
        }
    }
    (transcript, replacements)
}

/// Parse JSONL bytes (lossy UTF-8) into JSON-object lines.
pub(crate) fn parse_jsonl_objects(content: &[u8]) -> Vec<Entry> {
    String::from_utf8_lossy(content)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(entry)) => Some(entry),
            _ => None,
        })
        .collect()
}

/// Disk-path title derivation over the source file's 64 KiB head/tail.
pub(crate) fn derive_title_from_bytes(content: &[u8]) -> Option<String> {
    let len = content.len();
    let head = String::from_utf8_lossy(&content[..len.min(lite::LITE_READ_BUF_SIZE)]);
    let tail = String::from_utf8_lossy(&content[len.saturating_sub(lite::LITE_READ_BUF_SIZE)..]);
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    non_empty(lite::extract_last_json_string_field(&tail, "customTitle"))
        .or_else(|| non_empty(lite::extract_last_json_string_field(&head, "customTitle")))
        .or_else(|| non_empty(lite::extract_last_json_string_field(&tail, "aiTitle")))
        .or_else(|| non_empty(lite::extract_last_json_string_field(&head, "aiTitle")))
        .or_else(|| non_empty(Some(lite::extract_first_prompt_from_head(&head))))
}

/// `_derive_title_from_entries`: last non-empty `customTitle`, else last
/// `aiTitle`, else the first prompt of the re-serialized entries.
pub(crate) fn derive_title_from_entries(raw: &[Entry]) -> Option<String> {
    let last = |key: &str| {
        raw.iter()
            .rev()
            .filter_map(|entry| entry.get(key).and_then(Value::as_str))
            .find(|value| !value.is_empty())
            .map(String::from)
    };
    if let Some(custom) = last("customTitle") {
        return Some(custom);
    }
    if let Some(ai) = last("aiTitle") {
        return Some(ai);
    }
    let mut jsonl = raw
        .iter()
        .map(py_json::dumps_compact)
        .collect::<Vec<_>>()
        .join("\n");
    jsonl.push('\n');
    Some(lite::extract_first_prompt_from_head(&jsonl)).filter(|prompt| !prompt.is_empty())
}

fn no_messages(session_id: &str) -> ClaudeSDKError {
    ClaudeSDKError::Session(format!("Session {session_id} has no messages to fork"))
}

/// `_build_fork_lines`: returns `(forked_session_id, entries)`; each entry is
/// one line of the fork in key order.
pub(crate) fn build_fork_entries(
    transcript: Vec<Entry>,
    content_replacements: Vec<Value>,
    session_id: &str,
    up_to_message_id: Option<&str>,
    title: Option<&str>,
    derive_title: impl FnOnce() -> Option<String>,
) -> Result<(String, Vec<Entry>)> {
    let mut transcript: Vec<Entry> = transcript
        .into_iter()
        .filter(|entry| !truthy(entry.get("isSidechain")))
        .collect();
    if transcript.is_empty() {
        return Err(no_messages(session_id));
    }

    if let Some(up_to) = up_to_message_id.filter(|id| !id.is_empty()) {
        let cutoff = transcript
            .iter()
            .position(|entry| entry.get("uuid").and_then(Value::as_str) == Some(up_to))
            .ok_or_else(|| {
                ClaudeSDKError::Session(format!(
                    "Message {up_to} not found in session {session_id}"
                ))
            })?;
        transcript.truncate(cutoff + 1);
    }

    let uuid_of = |entry: &Entry| {
        entry
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut uuid_mapping: HashMap<String, String> = HashMap::new();
    let mut by_uuid: HashMap<String, usize> = HashMap::new();
    for (idx, entry) in transcript.iter().enumerate() {
        uuid_mapping.insert(uuid_of(entry), uuid::Uuid::new_v4().to_string());
        by_uuid.insert(uuid_of(entry), idx);
    }

    let writable: Vec<&Entry> = transcript
        .iter()
        .filter(|entry| entry.get("type").and_then(Value::as_str) != Some("progress"))
        .collect();
    if writable.is_empty() {
        return Err(no_messages(session_id));
    }

    let forked_session_id = uuid::Uuid::new_v4().to_string();
    let now = iso_now();
    let mut lines = Vec::with_capacity(writable.len() + 2);
    let last_idx = writable.len() - 1;

    for (i, original) in writable.into_iter().enumerate() {
        let original_uuid = uuid_of(original);
        let new_uuid = uuid_mapping[&original_uuid].clone();

        // Resolve parentUuid, skipping progress ancestors (cycle-safe).
        let mut new_parent_uuid = Value::Null;
        let mut parent_id = original.get("parentUuid").cloned();
        let mut visited = HashSet::new();
        while let Some(parent) = parent_id.as_ref().filter(|value| truthy(Some(value))) {
            let Some(parent_key) = parent.as_str() else {
                break;
            };
            if !visited.insert(parent_key.to_string()) {
                break;
            }
            let Some(&parent_idx) = by_uuid.get(parent_key) else {
                break;
            };
            let parent_entry = &transcript[parent_idx];
            if parent_entry.get("type").and_then(Value::as_str) != Some("progress") {
                new_parent_uuid = uuid_mapping
                    .get(parent_key)
                    .map(|mapped| Value::String(mapped.clone()))
                    .unwrap_or(Value::Null);
                break;
            }
            parent_id = parent_entry.get("parentUuid").cloned();
        }

        let timestamp = if i == last_idx {
            Value::String(now.clone())
        } else {
            original
                .get("timestamp")
                .cloned()
                .unwrap_or_else(|| Value::String(now.clone()))
        };

        let logical_parent = original.get("logicalParentUuid");
        let new_logical_parent = if truthy(logical_parent) {
            logical_parent
                .and_then(Value::as_str)
                .and_then(|old| uuid_mapping.get(old))
                .map(|mapped| Value::String(mapped.clone()))
                .unwrap_or(Value::Null)
        } else {
            logical_parent.cloned().unwrap_or(Value::Null)
        };

        let mut forked = original.clone();
        forked.insert("uuid".to_string(), Value::String(new_uuid));
        forked.insert("parentUuid".to_string(), new_parent_uuid);
        forked.insert("logicalParentUuid".to_string(), new_logical_parent);
        forked.insert(
            "sessionId".to_string(),
            Value::String(forked_session_id.clone()),
        );
        forked.insert("timestamp".to_string(), timestamp);
        forked.insert("isSidechain".to_string(), Value::Bool(false));
        forked.insert(
            "forkedFrom".to_string(),
            json!({"sessionId": session_id, "messageUuid": original_uuid}),
        );
        for key in ["teamName", "agentName", "slug", "sourceToolAssistantUUID"] {
            forked.shift_remove(key);
        }
        lines.push(forked);
    }

    if !content_replacements.is_empty() {
        lines.push(
            json!({
                "type": "content-replacement",
                "sessionId": forked_session_id,
                "replacements": content_replacements,
                "uuid": uuid::Uuid::new_v4().to_string(),
                "timestamp": now,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
    }

    let fork_title = match title
        .map(crate::sessions::sanitize::py_strip)
        .filter(|title| !title.is_empty())
    {
        Some(title) => title.to_string(),
        None => format!(
            "{} (fork)",
            derive_title().unwrap_or_else(|| "Forked session".to_string())
        ),
    };
    lines.push(
        json!({
            "type": "custom-title",
            "sessionId": forked_session_id,
            "customTitle": fork_title,
            "uuid": uuid::Uuid::new_v4().to_string(),
            "timestamp": now,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    );

    Ok((forked_session_id, lines))
}
