//! Lite (head/tail) session metadata parsing shared by the local-disk and
//! `SessionStore` paths. Ports the string-scanning helpers and
//! `_parse_session_info_from_lite` from upstream `_internal/sessions.py`.

use crate::sessions::py_json;
use crate::sessions::SDKSessionInfo;
use chrono::TimeZone;
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Size of the head/tail buffer for lite metadata reads.
pub(crate) const LITE_READ_BUF_SIZE: usize = 65536;

/// Result of reading a session file's head, tail, mtime and size.
#[derive(Debug, Clone)]
pub(crate) struct LiteSessionFile {
    pub mtime: i64,
    pub size: i64,
    pub head: String,
    pub tail: String,
}

/// `_read_session_lite`: stat + first/last 64 KiB. `None` on any error or for
/// an empty file.
pub(crate) fn read_session_lite(file_path: &Path) -> Option<LiteSessionFile> {
    let mut file = std::fs::File::open(file_path).ok()?;
    let metadata = file.metadata().ok()?;
    let size = metadata.len();
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0);

    let mut head_bytes = Vec::with_capacity(LITE_READ_BUF_SIZE.min(size as usize));
    (&mut file)
        .take(LITE_READ_BUF_SIZE as u64)
        .read_to_end(&mut head_bytes)
        .ok()?;
    if head_bytes.is_empty() {
        return None;
    }
    let head = String::from_utf8_lossy(&head_bytes).into_owned();

    let tail_offset = size.saturating_sub(LITE_READ_BUF_SIZE as u64);
    let tail = if tail_offset == 0 {
        head.clone()
    } else {
        file.seek(SeekFrom::Start(tail_offset)).ok()?;
        let mut tail_bytes = Vec::with_capacity(LITE_READ_BUF_SIZE);
        file.take(LITE_READ_BUF_SIZE as u64)
            .read_to_end(&mut tail_bytes)
            .ok()?;
        String::from_utf8_lossy(&tail_bytes).into_owned()
    };

    Some(LiteSessionFile {
        mtime,
        size: size as i64,
        head,
        tail,
    })
}

/// `_unescape_json_string`: decode a raw JSON string body extracted as text.
pub(crate) fn unescape_json_string(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    match serde_json::from_str::<Value>(&format!("\"{raw}\"")) {
        Ok(Value::String(value)) => value,
        _ => raw.to_string(),
    }
}

/// Scan for the closing quote of a raw JSON string starting at `start`,
/// skipping backslash escapes. Returns the index of the quote, or the scan end.
fn scan_string_end(bytes: &[u8], start: usize) -> (Option<usize>, usize) {
    let mut idx = start;
    while idx < bytes.len() {
        match bytes[idx] {
            b'\\' => idx += 2,
            b'"' => return (Some(idx), idx),
            _ => idx += 1,
        }
    }
    (None, idx)
}

fn field_patterns(key: &str) -> [String; 2] {
    [format!("\"{key}\":\""), format!("\"{key}\": \"")]
}

/// `_extract_json_string_field`: first `"key":"value"` (or `"key": "value"`).
pub(crate) fn extract_json_string_field(text: &str, key: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for pattern in field_patterns(key) {
        let Some(idx) = text.find(&pattern) else {
            continue;
        };
        let value_start = idx + pattern.len();
        if let (Some(end), _) = scan_string_end(bytes, value_start) {
            return Some(unescape_json_string(&text[value_start..end]));
        }
    }
    None
}

/// `_extract_last_json_string_field`: like [`extract_json_string_field`] but
/// returns the LAST occurrence (the second pattern form wins when both match).
pub(crate) fn extract_last_json_string_field(text: &str, key: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut last_value = None;
    for pattern in field_patterns(key) {
        let mut search_from = 0;
        while search_from <= text.len() {
            let Some(found) = text[search_from..].find(&pattern) else {
                break;
            };
            let value_start = search_from + found + pattern.len();
            let (end, scanned) = scan_string_end(bytes, value_start);
            if let Some(end) = end {
                last_value = Some(unescape_json_string(&text[value_start..end]));
            }
            search_from = scanned + 1;
            while search_from < text.len() && !text.is_char_boundary(search_from) {
                search_from += 1;
            }
        }
    }
    last_value
}

/// `_COMMAND_NAME_RE.search(text)` -> group 1.
pub(crate) fn command_name(text: &str) -> Option<String> {
    const OPEN: &str = "<command-name>";
    const CLOSE: &str = "</command-name>";
    let start = text.find(OPEN)? + OPEN.len();
    let end = text[start..].find(CLOSE)? + start;
    Some(text[start..end].to_string())
}

/// `_SKIP_FIRST_PROMPT_PATTERN.match(text)` for an already-stripped prompt.
pub(crate) fn should_skip_first_prompt(text: &str) -> bool {
    const PREFIXES: [&str; 4] = [
        "<local-command-stdout>",
        "<session-start-hook>",
        "<tick>",
        "<goal>",
    ];
    if PREFIXES.iter().any(|prefix| text.starts_with(prefix)) {
        return true;
    }
    if let Some(rest) = text.strip_prefix("[Request interrupted by user") {
        if rest.contains(']') {
            return true;
        }
    }
    let wrapped = |open: &str, close: &str| {
        let trimmed = text.trim_start();
        trimmed.starts_with(open) && trimmed.trim_end().ends_with(close)
    };
    wrapped("<ide_opened_file>", "</ide_opened_file>")
        || wrapped("<ide_selection>", "</ide_selection>")
}

/// First-prompt truncation: more than 200 code points -> first 200, rstrip,
/// then U+2026.
pub(crate) fn truncate_prompt(text: String) -> String {
    if text.chars().count() <= 200 {
        return text;
    }
    let mut truncated: String = text.chars().take(200).collect();
    truncated.truncate(crate::sessions::sanitize::py_rstrip(&truncated).len());
    truncated.push('\u{2026}');
    truncated
}

/// Text strings of a user entry's `message.content` (string or text blocks).
pub(crate) fn entry_text_blocks(entry: &serde_json::Map<String, Value>) -> Vec<String> {
    let Some(message) = entry.get("message").and_then(Value::as_object) else {
        return Vec::new();
    };
    match message.get("content") {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(Value::as_object)
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// `_extract_first_prompt_from_head`. Returns `""` when nothing qualifies.
pub(crate) fn extract_first_prompt_from_head(head: &str) -> String {
    let mut command_fallback = String::new();
    for line in head.split('\n') {
        if !line.contains("\"type\":\"user\"") && !line.contains("\"type\": \"user\"") {
            continue;
        }
        if line.contains("\"tool_result\"")
            || line.contains("\"isMeta\":true")
            || line.contains("\"isMeta\": true")
            || line.contains("\"isCompactSummary\":true")
            || line.contains("\"isCompactSummary\": true")
        {
            continue;
        }
        let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        for raw in entry_text_blocks(&entry) {
            let result = crate::sessions::sanitize::py_strip(&raw.replace('\n', " ")).to_string();
            if result.is_empty() {
                continue;
            }
            if let Some(command) = command_name(&result) {
                if command_fallback.is_empty() {
                    command_fallback = command;
                }
                continue;
            }
            if should_skip_first_prompt(&result) {
                continue;
            }
            return truncate_prompt(result);
        }
    }
    command_fallback
}

/// Python `datetime.fromisoformat(ts)` (trailing `Z` accepted) -> epoch ms.
/// Offset-less values are interpreted in local time, like Python.
pub(crate) fn iso_to_epoch_ms(timestamp: &str) -> Option<i64> {
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp) {
        return Some(parsed.timestamp_millis());
    }
    let normalized = match timestamp.strip_suffix('Z') {
        Some(stripped) => format!("{stripped}+00:00"),
        None => timestamp.to_string(),
    };
    const OFFSET_FORMATS: [&str; 8] = [
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%dT%H:%M%:z",
        "%Y-%m-%dT%H:%M%z",
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%.f%z",
        "%Y-%m-%d %H:%M%:z",
        "%Y-%m-%d %H:%M%z",
    ];
    for format in OFFSET_FORMATS {
        if let Ok(parsed) = chrono::DateTime::parse_from_str(&normalized, format) {
            return Some(parsed.timestamp_millis());
        }
    }
    const NAIVE_FORMATS: [&str; 4] = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
    ];
    let naive = NAIVE_FORMATS
        .iter()
        .find_map(|format| chrono::NaiveDateTime::parse_from_str(&normalized, format).ok())
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(&normalized, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
        })?;
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|local| local.timestamp_millis())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// `_parse_session_info_from_lite`. `None` for sidechain sessions and
/// metadata-only sessions with no extractable summary.
pub(crate) fn parse_session_info_from_lite(
    session_id: &str,
    lite: &LiteSessionFile,
    project_path: Option<&str>,
) -> Option<SDKSessionInfo> {
    let head = lite.head.as_str();
    let tail = lite.tail.as_str();

    let first_line = head.split('\n').next().unwrap_or_default();
    if first_line.contains("\"isSidechain\":true") || first_line.contains("\"isSidechain\": true") {
        return None;
    }

    let custom_title = non_empty(extract_last_json_string_field(tail, "customTitle"))
        .or_else(|| non_empty(extract_last_json_string_field(head, "customTitle")))
        .or_else(|| non_empty(extract_last_json_string_field(tail, "aiTitle")))
        .or_else(|| non_empty(extract_last_json_string_field(head, "aiTitle")));
    let first_prompt = non_empty(Some(extract_first_prompt_from_head(head)));
    let summary = custom_title
        .clone()
        .or_else(|| non_empty(extract_last_json_string_field(tail, "lastPrompt")))
        .or_else(|| non_empty(extract_last_json_string_field(tail, "summary")))
        .or_else(|| first_prompt.clone())?;

    let git_branch = non_empty(extract_last_json_string_field(tail, "gitBranch"))
        .or_else(|| non_empty(extract_json_string_field(head, "gitBranch")));
    let cwd = non_empty(extract_json_string_field(head, "cwd")).or_else(|| {
        project_path
            .filter(|path| !path.is_empty())
            .map(String::from)
    });
    let tag = tail
        .split('\n')
        .rev()
        .find(|line| line.starts_with("{\"type\":\"tag\""))
        .and_then(|line| non_empty(extract_last_json_string_field(line, "tag")));
    let created_at = extract_json_string_field(head, "timestamp")
        .filter(|timestamp| !timestamp.is_empty())
        .and_then(|timestamp| iso_to_epoch_ms(&timestamp));

    Some(SDKSessionInfo {
        session_id: session_id.to_string(),
        summary,
        last_modified: lite.mtime,
        file_size: Some(lite.size),
        custom_title,
        first_prompt,
        git_branch,
        cwd,
        tag,
        created_at,
    })
}

/// `_entries_to_jsonl`: compact Python JSON, one entry per line, with `type`
/// hoisted to the first key, `\n`-joined plus a trailing newline.
pub(crate) fn entries_to_jsonl(entries: &[serde_json::Map<String, Value>]) -> String {
    let mut out = String::new();
    for (idx, entry) in entries.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        match entry.get("type") {
            Some(entry_type) => {
                let mut hoisted = serde_json::Map::with_capacity(entry.len());
                hoisted.insert("type".to_string(), entry_type.clone());
                for (key, value) in entry {
                    if key != "type" {
                        hoisted.insert(key.clone(), value.clone());
                    }
                }
                out.push_str(&py_json::dumps_compact(&hoisted));
            }
            None => out.push_str(&py_json::dumps_compact(entry)),
        }
    }
    out.push('\n');
    out
}

/// `_jsonl_to_lite`: head/tail slices of an in-memory JSONL string.
pub(crate) fn jsonl_to_lite(jsonl: &str, mtime: i64) -> LiteSessionFile {
    let bytes = jsonl.as_bytes();
    let size = bytes.len();
    let head = String::from_utf8_lossy(&bytes[..size.min(LITE_READ_BUF_SIZE)]).into_owned();
    let tail = if size > LITE_READ_BUF_SIZE {
        String::from_utf8_lossy(&bytes[size - LITE_READ_BUF_SIZE..]).into_owned()
    } else {
        head.clone()
    };
    LiteSessionFile {
        mtime,
        size: size as i64,
        head,
        tail,
    }
}

/// `_mtime_from_jsonl_tail`: the last line's `timestamp`, else now.
pub(crate) fn mtime_from_jsonl_tail(jsonl: &str) -> i64 {
    let trimmed = jsonl.trim_end();
    let last_line = &trimmed[trimmed.rfind('\n').map(|idx| idx + 1).unwrap_or(0)..];
    serde_json::from_str::<Value>(last_line)
        .ok()
        .and_then(|value| {
            value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(iso_to_epoch_ms)
        })
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_first_and_last_string_fields() {
        let text = "{\"customTitle\":\"one\"}\n{\"customTitle\": \"two\"}\n{\"customTitle\":\"th\\\"ree\"}";
        assert_eq!(
            extract_json_string_field(text, "customTitle").as_deref(),
            Some("one")
        );
        // Python scans the compact pattern fully, then the spaced one: the
        // spaced match is last.
        assert_eq!(
            extract_last_json_string_field(text, "customTitle").as_deref(),
            Some("two")
        );
        assert_eq!(
            extract_last_json_string_field("{\"a\":\"x\\u00e9\"}", "a").as_deref(),
            Some("x\u{e9}")
        );
        assert_eq!(extract_last_json_string_field("{\"a\":\"open", "a"), None);
    }

    #[test]
    fn first_prompt_skips_meta_commands_and_truncates_by_code_points() {
        let long = "\u{4f60}".repeat(250);
        let head = [
            r#"{"type":"user","isMeta":true,"message":{"content":"meta"}}"#.to_string(),
            r#"{"type":"user","message":{"content":"<command-name>init</command-name>"}}"#
                .to_string(),
            r#"{"type":"user","message":{"content":"[Request interrupted by user]"}}"#.to_string(),
            format!(r#"{{"type":"user","message":{{"content":"{long}"}}}}"#),
        ]
        .join("\n");
        let prompt = extract_first_prompt_from_head(&head);
        assert_eq!(prompt.chars().count(), 201);
        assert!(prompt.ends_with('\u{2026}'));

        let only_command =
            r#"{"type":"user","message":{"content":"<command-name>init</command-name>"}}"#;
        assert_eq!(extract_first_prompt_from_head(only_command), "init");
        assert!(!should_skip_first_prompt("[Request interrupted by user"));
    }

    #[test]
    fn iso_parse_accepts_python_fromisoformat_shapes() {
        assert_eq!(
            iso_to_epoch_ms("2026-01-01T00:00:00Z"),
            Some(1_767_225_600_000)
        );
        assert_eq!(
            iso_to_epoch_ms("2026-01-01T00:00:00.123456+00:00"),
            Some(1_767_225_600_123)
        );
        assert!(iso_to_epoch_ms("2026-01-01T00:00:00").is_some());
        assert!(iso_to_epoch_ms("2026-01-01").is_some());
        assert_eq!(iso_to_epoch_ms("not a date"), None);
    }

    #[test]
    fn entries_to_jsonl_hoists_type() {
        let entry = serde_json::json!({"tag": "x", "type": "tag", "sessionId": "s"});
        let jsonl = entries_to_jsonl(&[entry.as_object().unwrap().clone()]);
        assert_eq!(
            jsonl,
            "{\"type\":\"tag\",\"tag\":\"x\",\"sessionId\":\"s\"}\n"
        );
    }
}
