//! Wire JSON -> typed [`Message`], mirroring the Python SDK's
//! `_internal/message_parser.py`.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{ClaudeSDKError, MessageParseError, Result};
use crate::types::messages::{
    is_falsy, py_type_name, KNOWN_ASSISTANT_BLOCK_TYPES, KNOWN_USER_BLOCK_TYPES,
};
use crate::types::{
    ContentBlock, HookEventMessage, Message, MirrorErrorMessage, TaskNotificationMessage,
    TaskProgressMessage, TaskStartedMessage, TaskUpdatedMessage, TaskUpdatedStatus,
};

type JsonMap = serde_json::Map<String, Value>;

const KNOWN_MESSAGE_TYPES: &[&str] = &[
    "user",
    "assistant",
    "system",
    "result",
    "stream_event",
    "rate_limit_event",
    "conversation_reset",
];

pub fn parse_message_line(line: &str) -> Result<Option<Message>> {
    let value = serde_json::from_str::<Value>(line)?;
    parse_message_value(value)
}

pub fn parse_message_value(value: Value) -> Result<Option<Message>> {
    let Value::Object(data) = &value else {
        return Err(ClaudeSDKError::MessageParse(MessageParseError::new(
            format!(
                "Invalid message data type (expected dict, got {})",
                py_type_name(&value)
            ),
        )));
    };

    // Hook events are routed before the generic `type` handling, like Python.
    if data.get("type").and_then(Value::as_str) == Some("system")
        && matches!(
            data.get("subtype").and_then(Value::as_str),
            Some("hook_started" | "hook_response")
        )
    {
        return Ok(Some(Message::HookEventMsg(parse_hook_event(data))));
    }

    let message_type = match data.get("type") {
        None => return Err(parse_error("Message missing 'type' field", data)),
        Some(value) if is_falsy(value) => {
            return Err(parse_error("Message missing 'type' field", data))
        }
        Some(Value::String(message_type)) => message_type.as_str(),
        // A truthy non-string type matches no known type: skip it.
        Some(_) => return Ok(None),
    };

    if !KNOWN_MESSAGE_TYPES.contains(&message_type) {
        // Forward-compatible: skip unrecognized message types.
        return Ok(None);
    }

    match message_type {
        "system" => parse_system_message(data).map(Some),
        "user" => {
            validate_user_message(data)?;
            deserialize_message(&value, "user").map(Some)
        }
        "assistant" => {
            let prepared = prepare_assistant_message(data)?;
            deserialize_message(&prepared, "assistant").map(Some)
        }
        other => deserialize_message(&value, other).map(Some),
    }
}

fn parse_error(message: impl Into<String>, data: &JsonMap) -> ClaudeSDKError {
    ClaudeSDKError::MessageParse(MessageParseError::new(message).with_data(data.clone()))
}

fn missing_field(message_type: &str, field: &str, data: &JsonMap) -> ClaudeSDKError {
    parse_error(
        format!("Missing required field in {message_type} message: '{field}'"),
        data,
    )
}

/// Extract the field name from a serde "missing field `x`" error.
fn missing_field_name(error: &str) -> Option<&str> {
    let rest = error.strip_prefix("missing field `")?;
    rest.split('`').next()
}

/// Map a serde error for a known message type to a `MessageParseError`,
/// using the Python wording for missing required fields.
fn map_serde_error(error: serde_json::Error, message_type: &str, data: &JsonMap) -> ClaudeSDKError {
    let text = error.to_string();
    if let Some(field) = missing_field_name(&text) {
        return missing_field(message_type, field, data);
    }
    if text.starts_with("Invalid content block") {
        return parse_error(text, data);
    }
    parse_error_with_payload(error, data)
}

fn deserialize_message(value: &Value, message_type: &str) -> Result<Message> {
    serde_json::from_value::<Message>(value.clone()).map_err(|error| {
        let data = value.as_object().cloned().unwrap_or_default();
        map_serde_error(error, message_type, &data)
    })
}

fn deserialize_system<T: DeserializeOwned>(data: &JsonMap) -> Result<T> {
    serde_json::from_value::<T>(Value::Object(data.clone()))
        .map_err(|error| map_serde_error(error, "system", data))
}

// Surface the offending payload alongside the serde error; a bare
// "invalid type: sequence, expected a map" is undebuggable without it.
fn parse_error_with_payload(err: serde_json::Error, data: &JsonMap) -> ClaudeSDKError {
    let payload = Value::Object(data.clone()).to_string();
    let payload = if payload.len() > 600 {
        let cut = payload
            .char_indices()
            .take_while(|(idx, _)| *idx <= 600)
            .last()
            .map(|(idx, ch)| idx + ch.len_utf8())
            .unwrap_or(payload.len());
        format!("{}...", &payload[..cut])
    } else {
        payload
    };
    parse_error(
        format!("Failed to parse CLI message: {err}; payload: {payload}"),
        data,
    )
}

/// Validate `content_blocks` the way the Python parser walks them: every
/// element must be an object with a `type`; known types must carry their
/// required keys. Unknown types are accepted here (and dropped later).
fn validate_blocks(
    blocks: &[Value],
    known: &[&str],
    message_type: &str,
    data: &JsonMap,
) -> Result<()> {
    for block in blocks {
        let Some(map) = block.as_object() else {
            return Err(parse_error(
                format!(
                    "Invalid content block (expected dict, got {})",
                    py_type_name(block)
                ),
                data,
            ));
        };
        let Some(block_type) = map.get("type") else {
            return Err(missing_field(message_type, "type", data));
        };
        if block_type
            .as_str()
            .is_some_and(|block_type| known.contains(&block_type))
        {
            if let Err(error) = serde_json::from_value::<ContentBlock>(block.clone()) {
                return Err(map_serde_error(error, message_type, data));
            }
        }
    }
    Ok(())
}

fn message_object<'a>(data: &'a JsonMap, message_type: &str) -> Result<&'a JsonMap> {
    match data.get("message") {
        None => Err(missing_field(message_type, "message", data)),
        Some(Value::Object(message)) => Ok(message),
        Some(other) => Err(parse_error(
            format!(
                "Invalid {message_type} message (expected dict, got {})",
                py_type_name(other)
            ),
            data,
        )),
    }
}

fn validate_user_message(data: &JsonMap) -> Result<()> {
    let message = message_object(data, "user")?;
    match message.get("content") {
        None => Err(missing_field("user", "content", data)),
        Some(Value::Array(blocks)) => validate_blocks(blocks, KNOWN_USER_BLOCK_TYPES, "user", data),
        Some(_) => Ok(()),
    }
}

/// Validate assistant content and copy `message.id` / `message.usage` /
/// `message.stop_reason` to the top-level `message_id` / `usage` /
/// `stop_reason` (Python reads them from the nested message).
fn prepare_assistant_message(data: &JsonMap) -> Result<Value> {
    let message = message_object(data, "assistant")?;
    match message.get("content") {
        None => return Err(missing_field("assistant", "content", data)),
        Some(Value::Array(blocks)) => {
            validate_blocks(blocks, KNOWN_ASSISTANT_BLOCK_TYPES, "assistant", data)?
        }
        Some(other) => {
            return Err(parse_error(
                format!(
                    "Invalid assistant content (expected list, got {})",
                    py_type_name(other)
                ),
                data,
            ))
        }
    }

    let mut prepared = data.clone();
    for (top_level, nested) in [
        ("message_id", "id"),
        ("usage", "usage"),
        ("stop_reason", "stop_reason"),
    ] {
        let top_missing = prepared.get(top_level).is_none_or(Value::is_null);
        if top_missing {
            if let Some(value) = message.get(nested).filter(|value| !value.is_null()) {
                prepared.insert(top_level.to_string(), value.clone());
            }
        }
    }
    Ok(Value::Object(prepared))
}

fn parse_system_message(data: &JsonMap) -> Result<Message> {
    let subtype = match data.get("subtype") {
        None => return Err(missing_field("system", "subtype", data)),
        Some(Value::String(subtype)) => subtype.clone(),
        Some(other) => other.to_string(),
    };
    match subtype.as_str() {
        "task_started" => {
            let mut message: TaskStartedMessage = deserialize_system(data)?;
            message.subtype = subtype;
            message.data = data.clone();
            Ok(Message::TaskStartedMsg(message))
        }
        "task_progress" => {
            let mut message: TaskProgressMessage = deserialize_system(data)?;
            message.subtype = subtype;
            message.data = data.clone();
            Ok(Message::TaskProgressMsg(message))
        }
        "task_notification" => {
            let mut message: TaskNotificationMessage = deserialize_system(data)?;
            message.subtype = subtype;
            message.data = data.clone();
            Ok(Message::TaskNotificationMsg(message))
        }
        "task_updated" => Ok(Message::TaskUpdatedMsg(parse_task_updated(subtype, data))),
        "mirror_error" => Ok(Message::MirrorErrorMsg(parse_mirror_error(data))),
        _ => Ok(Message::SystemMsg {
            subtype,
            data: data.clone(),
        }),
    }
}

// SDK-synthesized via report_mirror_error — never emitted by the CLI.
fn parse_mirror_error(data: &JsonMap) -> MirrorErrorMessage {
    let key = data.get("key").and_then(Value::as_object).cloned();
    let error = data
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    MirrorErrorMessage {
        key,
        error,
        data: data.clone(),
    }
}

// Parsed defensively: a terminal task completion sometimes arrives only as a
// `task_updated` patch (no separate `task_notification`), and the patch may omit
// uuid/session_id. `status` is derived from `patch.status` (the CLI sets it on
// terminal transitions); an unrecognized status string is kept in
// `TaskUpdatedStatus::Other` while the full patch is preserved on `.patch`.
fn parse_task_updated(subtype: String, data: &JsonMap) -> TaskUpdatedMessage {
    let task_id = data
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let patch = data
        .get("patch")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let status = patch
        .get("status")
        .and_then(Value::as_str)
        .map(TaskUpdatedStatus::from);
    let session_id = data
        .get("session_id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let uuid = data
        .get("uuid")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    TaskUpdatedMessage {
        subtype,
        task_id,
        patch,
        status,
        session_id,
        uuid,
        data: data.clone(),
    }
}

fn parse_hook_event(data: &JsonMap) -> HookEventMessage {
    let subtype = data
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // Python: `hook_event or hook_name or hook_event_name or ""`.
    let hook_event_name = ["hook_event", "hook_name", "hook_event_name"]
        .iter()
        .find_map(|key| {
            data.get(*key)
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_default()
        .to_string();
    let session_id = data
        .get("session_id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let uuid = data
        .get("uuid")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    HookEventMessage {
        subtype,
        hook_event_name,
        session_id,
        uuid,
        data: data.clone(),
    }
}
