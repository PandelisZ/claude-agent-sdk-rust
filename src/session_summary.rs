use crate::session_store::{SessionKey, SessionStoreEntry, SessionSummaryEntry};
use crate::sessions::chain::truthy;
use crate::sessions::{lite, SDKSessionInfo};

const LAST_WINS_FIELDS: &[(&str, &str)] = &[
    ("customTitle", "custom_title"),
    ("aiTitle", "ai_title"),
    ("lastPrompt", "last_prompt"),
    ("summary", "summary_hint"),
    ("gitBranch", "git_branch"),
];

pub fn fold_session_summary(
    prev: Option<&SessionSummaryEntry>,
    key: &SessionKey,
    entries: &[SessionStoreEntry],
) -> SessionSummaryEntry {
    let mut summary = prev.cloned().unwrap_or_else(|| SessionSummaryEntry {
        session_id: key.session_id.clone(),
        mtime: 0,
        data: serde_json::Map::new(),
    });

    for entry in entries {
        fold_entry(&mut summary.data, entry);
    }

    summary
}

pub fn summary_entry_to_sdk_info(
    entry: &SessionSummaryEntry,
    project_path: Option<&str>,
) -> Option<SDKSessionInfo> {
    let data = &entry.data;
    if truthy(data.get("is_sidechain")) {
        return None;
    }

    let first_prompt = if truthy(data.get("first_prompt_locked")) {
        string_data(data, "first_prompt")
    } else {
        string_data(data, "command_fallback")
    };
    let custom_title = string_data(data, "custom_title").or_else(|| string_data(data, "ai_title"));
    let summary = custom_title
        .clone()
        .or_else(|| string_data(data, "last_prompt"))
        .or_else(|| string_data(data, "summary_hint"))
        .or_else(|| first_prompt.clone())?;

    Some(SDKSessionInfo {
        session_id: entry.session_id.clone(),
        summary,
        last_modified: entry.mtime,
        file_size: None,
        custom_title,
        first_prompt,
        git_branch: string_data(data, "git_branch"),
        cwd: string_data(data, "cwd").or_else(|| {
            project_path
                .filter(|path| !path.is_empty())
                .map(String::from)
        }),
        tag: string_data(data, "tag"),
        created_at: data.get("created_at").and_then(|value| value.as_i64()),
    })
}

fn string_data(data: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(String::from)
}

fn fold_entry(data: &mut serde_json::Map<String, serde_json::Value>, entry: &SessionStoreEntry) {
    if !data.contains_key("is_sidechain") {
        data.insert(
            "is_sidechain".to_string(),
            serde_json::Value::Bool(
                entry.get("isSidechain").and_then(|v| v.as_bool()) == Some(true),
            ),
        );
    }

    if !data.contains_key("created_at") {
        if let Some(timestamp) = entry.get("timestamp").and_then(|v| v.as_str()) {
            if let Some(ms) = iso_to_epoch_ms(timestamp) {
                data.insert("created_at".to_string(), serde_json::json!(ms));
            }
        }
    }

    if !data.contains_key("cwd") {
        if let Some(cwd) = entry
            .get("cwd")
            .and_then(|v| v.as_str())
            .filter(|cwd| !cwd.is_empty())
        {
            data.insert(
                "cwd".to_string(),
                serde_json::Value::String(cwd.to_string()),
            );
        }
    }

    fold_first_prompt(data, entry);

    for (src, dst) in LAST_WINS_FIELDS {
        if let Some(value) = entry.get(*src).and_then(|v| v.as_str()) {
            data.insert(
                (*dst).to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }

    if entry.get("type").and_then(|v| v.as_str()) == Some("tag") {
        match entry
            .get("tag")
            .and_then(|v| v.as_str())
            .filter(|tag| !tag.is_empty())
        {
            Some(tag) => {
                data.insert(
                    "tag".to_string(),
                    serde_json::Value::String(tag.to_string()),
                );
            }
            None => {
                // Order-preserving, like Python's `dict.pop`.
                data.shift_remove("tag");
            }
        }
    }
}

fn iso_to_epoch_ms(timestamp: &str) -> Option<i64> {
    lite::iso_to_epoch_ms(timestamp)
}

/// Replicates `_extract_first_prompt_from_head` for a single parsed entry.
fn fold_first_prompt(
    data: &mut serde_json::Map<String, serde_json::Value>,
    entry: &SessionStoreEntry,
) {
    if truthy(data.get("first_prompt_locked")) {
        return;
    }
    if entry.get("type").and_then(|v| v.as_str()) != Some("user") {
        return;
    }
    if entry.get("isMeta").and_then(|v| v.as_bool()) == Some(true)
        || entry.get("isCompactSummary").and_then(|v| v.as_bool()) == Some(true)
        || carries_tool_result(entry)
    {
        return;
    }

    for raw in lite::entry_text_blocks(entry) {
        let prompt = crate::sessions::sanitize::py_strip(&raw.replace('\n', " ")).to_string();
        if prompt.is_empty() {
            continue;
        }
        if let Some(command) = lite::command_name(&prompt) {
            if !truthy(data.get("command_fallback")) {
                data.insert(
                    "command_fallback".to_string(),
                    serde_json::Value::String(command),
                );
            }
            continue;
        }
        if lite::should_skip_first_prompt(&prompt) {
            continue;
        }
        data.insert(
            "first_prompt".to_string(),
            serde_json::Value::String(lite::truncate_prompt(prompt)),
        );
        data.insert(
            "first_prompt_locked".to_string(),
            serde_json::Value::Bool(true),
        );
        return;
    }
}

fn carries_tool_result(entry: &SessionStoreEntry) -> bool {
    entry
        .get("message")
        .and_then(|v| v.as_object())
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block
                    .as_object()
                    .and_then(|block| block.get("type"))
                    .and_then(|v| v.as_str())
                    == Some("tool_result")
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SessionKey {
        SessionKey {
            project_key: "proj".to_string(),
            session_id: "session".to_string(),
            subpath: None,
        }
    }

    #[test]
    fn folds_first_prompt_and_last_wins_fields() {
        let entries = vec![
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-05-08T10:00:00Z",
                "cwd": "/repo",
                "message": {"content": "<command-name>init</command-name>"}
            })
            .as_object()
            .unwrap()
            .clone(),
            serde_json::json!({
                "type": "user",
                "lastPrompt": "latest prompt",
                "gitBranch": "main",
                "message": {"content": [{"type": "text", "text": "Real prompt"}]}
            })
            .as_object()
            .unwrap()
            .clone(),
        ];

        let summary = fold_session_summary(None, &key(), &entries);

        assert_eq!(summary.session_id, "session");
        assert_eq!(summary.data["created_at"], 1_778_234_400_000i64);
        assert_eq!(summary.data["cwd"], "/repo");
        assert_eq!(summary.data["command_fallback"], "init");
        assert_eq!(summary.data["first_prompt"], "Real prompt");
        assert_eq!(summary.data["last_prompt"], "latest prompt");
        assert_eq!(summary.data["git_branch"], "main");
    }

    #[test]
    fn truncates_multibyte_first_prompt_by_code_points() {
        // Byte 200 of this prompt is not a char boundary; the old byte-based
        // truncate panicked here.
        let prompt = format!("a{}", "\u{4f60}".repeat(300));
        let entry = serde_json::json!({"type": "user", "message": {"content": prompt}})
            .as_object()
            .unwrap()
            .clone();
        let summary = fold_session_summary(None, &key(), &[entry]);
        let first = summary.data["first_prompt"].as_str().unwrap();
        assert_eq!(first.chars().count(), 201);
        assert!(first.ends_with('\u{2026}'));

        let interrupted = serde_json::json!({
            "type": "user",
            "message": {"content": "[Request interrupted by user"}
        })
        .as_object()
        .unwrap()
        .clone();
        let summary = fold_session_summary(None, &key(), &[interrupted]);
        // No closing bracket: the skip pattern does not match.
        assert_eq!(summary.data["first_prompt"], "[Request interrupted by user");
    }

    #[test]
    fn tag_entries_set_and_clear_tag() {
        let set_tag = serde_json::json!({"type": "tag", "tag": "important"})
            .as_object()
            .unwrap()
            .clone();
        let clear_tag = serde_json::json!({"type": "tag", "tag": ""})
            .as_object()
            .unwrap()
            .clone();

        let with_tag = fold_session_summary(None, &key(), &[set_tag]);
        assert_eq!(with_tag.data["tag"], "important");

        let without_tag = fold_session_summary(Some(&with_tag), &key(), &[clear_tag]);
        assert!(without_tag.data.get("tag").is_none());
    }
}
