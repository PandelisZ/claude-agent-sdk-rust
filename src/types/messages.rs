use serde::{Deserialize, Serialize};

/// Names of server-side tools the API executes on the model's behalf.
///
/// Mirrors the upstream Python `ServerToolName` literal. Deserializes
/// forward-compatibly: any unrecognized name is preserved in
/// [`ServerToolName::Other`] rather than failing to parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ServerToolName {
    Advisor,
    WebSearch,
    WebFetch,
    CodeExecution,
    BashCodeExecution,
    TextEditorCodeExecution,
    ToolSearchToolRegex,
    ToolSearchToolBm25,
    /// A server tool name not known at build time (forward-compatible).
    Other(String),
}

impl ServerToolName {
    /// The wire string for this server tool name.
    pub fn as_str(&self) -> &str {
        match self {
            ServerToolName::Advisor => "advisor",
            ServerToolName::WebSearch => "web_search",
            ServerToolName::WebFetch => "web_fetch",
            ServerToolName::CodeExecution => "code_execution",
            ServerToolName::BashCodeExecution => "bash_code_execution",
            ServerToolName::TextEditorCodeExecution => "text_editor_code_execution",
            ServerToolName::ToolSearchToolRegex => "tool_search_tool_regex",
            ServerToolName::ToolSearchToolBm25 => "tool_search_tool_bm25",
            ServerToolName::Other(name) => name.as_str(),
        }
    }
}

impl From<String> for ServerToolName {
    fn from(value: String) -> Self {
        match value.as_str() {
            "advisor" => ServerToolName::Advisor,
            "web_search" => ServerToolName::WebSearch,
            "web_fetch" => ServerToolName::WebFetch,
            "code_execution" => ServerToolName::CodeExecution,
            "bash_code_execution" => ServerToolName::BashCodeExecution,
            "text_editor_code_execution" => ServerToolName::TextEditorCodeExecution,
            "tool_search_tool_regex" => ServerToolName::ToolSearchToolRegex,
            "tool_search_tool_bm25" => ServerToolName::ToolSearchToolBm25,
            _ => ServerToolName::Other(value),
        }
    }
}

impl From<ServerToolName> for String {
    fn from(value: ServerToolName) -> Self {
        match value {
            ServerToolName::Other(name) => name,
            other => other.as_str().to_string(),
        }
    }
}

use std::collections::HashMap;

use serde::de::DeserializeOwned;

use super::{
    AssistantMessageErrorKind, PermissionMode, RateLimitInfo, TaskNotificationStatus,
    TaskUpdatedStatus, TaskUsage,
};
use crate::session_store::SessionKey;

type JsonMap = serde_json::Map<String, serde_json::Value>;

/// Content block types the parser keeps on assistant messages. Every other
/// block type (`redacted_thinking`, `web_search_tool_result`, `image`, any
/// future type, ...) is dropped, matching the Python parser.
pub(crate) const KNOWN_ASSISTANT_BLOCK_TYPES: &[&str] = &[
    "text",
    "thinking",
    "tool_use",
    "tool_result",
    "server_tool_use",
    "advisor_tool_result",
];

/// Content block types the parser keeps on user messages with list content.
pub(crate) const KNOWN_USER_BLOCK_TYPES: &[&str] = &["text", "tool_use", "tool_result"];

// ---------------------------------------------------------------------------
// Lenient serde helpers
// ---------------------------------------------------------------------------

/// Deserialize an optional value, falling back to `None` when the wire value
/// has an unexpected shape (Python passes such values through untouched; the
/// typed Rust field cannot hold them, so it must at least not fail the whole
/// message).
fn lenient<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

fn lenient_origin<'de, D>(deserializer: D) -> std::result::Result<Option<MessageOrigin>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.as_ref().and_then(MessageOrigin::from_value))
}

/// `errors` on a result frame: the CLI emits a list of strings; tolerate a
/// bare string and drop non-string entries instead of failing the message.
fn lenient_errors<'de, D>(deserializer: D) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(error)) => Some(vec![error]),
        Some(serde_json::Value::Array(items)) => Some(
            items
                .into_iter()
                .filter_map(|item| match item {
                    serde_json::Value::String(error) => Some(error),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    })
}

/// `deferred_tool_use`: Python treats any falsy value (`null`, `{}`, ...) as
/// absent; anything else must be a full `DeferredToolUse`.
fn falsy_as_none<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        Some(value) if !is_falsy(&value) => serde_json::from_value(value)
            .map(Some)
            .map_err(serde::de::Error::custom),
        _ => Ok(None),
    }
}

/// Integer counters (`duration_ms`, `num_turns`, ...): accept integral floats.
fn int_from_number<'de, D>(deserializer: D) -> std::result::Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Number::deserialize(deserializer)?;
    if let Some(int) = value.as_i64() {
        return Ok(int);
    }
    match value.as_f64() {
        Some(float) if float.fract() == 0.0 && float.is_finite() => Ok(float as i64),
        _ => Err(serde::de::Error::custom(format!(
            "invalid type: number {value}, expected an integer"
        ))),
    }
}

/// Python truthiness for a JSON value.
pub(crate) fn is_falsy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(flag) => !flag,
        serde_json::Value::Number(number) => number.as_f64() == Some(0.0),
        serde_json::Value::String(text) => text.is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Object(map) => map.is_empty(),
    }
}

/// Python `type(x).__name__` for a JSON value, used in parse error messages.
pub(crate) fn py_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "NoneType",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(number) if number.is_f64() => "float",
        serde_json::Value::Number(_) => "int",
        serde_json::Value::String(_) => "str",
        serde_json::Value::Array(_) => "list",
        serde_json::Value::Object(_) => "dict",
    }
}

/// Keep only the content blocks the Python parser models; unknown block types
/// are dropped instead of failing the whole message.
fn deserialize_assistant_blocks<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<ContentBlock>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let items = Vec::<serde_json::Value>::deserialize(deserializer)?;
    filter_content_blocks(items, KNOWN_ASSISTANT_BLOCK_TYPES).map_err(serde::de::Error::custom)
}

pub(crate) fn filter_content_blocks(
    items: Vec<serde_json::Value>,
    known: &[&str],
) -> std::result::Result<Vec<ContentBlock>, String> {
    let mut blocks = Vec::with_capacity(items.len());
    for item in items {
        let Some(block) = item.as_object() else {
            return Err(format!(
                "Invalid content block (expected dict, got {})",
                py_type_name(&item)
            ));
        };
        let Some(block_type) = block.get("type") else {
            return Err("missing field `type`".to_string());
        };
        if !block_type
            .as_str()
            .is_some_and(|block_type| known.contains(&block_type))
        {
            continue;
        }
        blocks.push(serde_json::from_value(item).map_err(|err| err.to_string())?);
    }
    Ok(blocks)
}

fn serialize_system_data<S>(data: &JsonMap, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    // `data` holds the full raw payload (including `type`/`subtype`), which
    // the enum tag and the `subtype` field already emit.
    serializer.collect_map(
        data.iter()
            .filter(|(key, _)| key.as_str() != "type" && key.as_str() != "subtype"),
    )
}

// ---------------------------------------------------------------------------
// Content blocks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Map<String, serde_json::Value>,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    ServerToolUse {
        id: String,
        name: ServerToolName,
        input: serde_json::Map<String, serde_json::Value>,
    },
    #[serde(rename = "advisor_tool_result")]
    ServerToolResult {
        tool_use_id: String,
        content: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Raw `message.content`: a string or a list of content blocks, as sent
    /// by the CLI. Use [`UserContent::text`] / [`UserContent::blocks`] for the
    /// typed view Python exposes as `UserMessage.content`.
    pub content: serde_json::Value,
}

impl UserContent {
    /// The content when it is a plain string.
    pub fn text(&self) -> Option<&str> {
        self.content.as_str()
    }

    /// The content when it is a list, as typed blocks.
    ///
    /// Mirrors the Python parser: only `text`, `tool_use` and `tool_result`
    /// blocks are kept; other block types (e.g. `image`) are dropped. Returns
    /// `None` when the content is not a list or a known block is malformed.
    pub fn blocks(&self) -> Option<Vec<ContentBlock>> {
        let items = self.content.as_array()?;
        filter_content_blocks(items.clone(), KNOWN_USER_BLOCK_TYPES).ok()
    }
}

// ---------------------------------------------------------------------------
// Message origin
// ---------------------------------------------------------------------------

open_string_enum! {
    /// Known values of [`MessageOrigin::kind`]. Newer CLI versions may emit
    /// kinds not listed here; treat anything unrecognized as "not human".
    pub enum MessageOriginKind {
        Human => "human",
        Channel => "channel",
        Peer => "peer",
        TaskNotification => "task-notification",
        Coordinator => "coordinator",
        Unclassified => "unclassified",
        Observer => "observer",
        AutoContinuation => "auto-continuation",
        ObserverActivity => "observer-activity",
    }
}

open_string_enum! {
    /// Values of [`MessageOrigin::subkind`] for `kind == "task-notification"`.
    pub enum TaskNotificationOriginSubkind {
        ScheduledTrigger => "scheduled-trigger",
        PeerSendMessage => "peer-send-message",
    }
}

/// Provenance of a user-role message, and — on a result message — of the
/// message that triggered that turn.
///
/// Only `kind` is always present; the remaining keys depend on `kind`. The
/// object is passed through from the CLI as-is: keys this SDK version does not
/// model are kept in `extra`. Parsed leniently: an `origin` that is not an
/// object with a string `kind` is treated as absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageOrigin {
    pub kind: MessageOriginKind,
    /// `kind == "channel"`: name of the MCP server the message arrived on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// `kind == "peer"` / `"observer"`: sender address (sender-asserted).
    #[serde(rename = "from", default, skip_serializing_if = "Option::is_none")]
    pub from_: Option<String>,
    /// `kind == "peer"`: sender display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `kind == "peer"`: the sender's host-openable session id.
    #[serde(
        rename = "fromSession",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub from_session: Option<String>,
    /// `kind == "peer"` / `"observer"`: task id of the sending subagent.
    #[serde(
        rename = "senderTaskId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sender_task_id: Option<String>,
    /// `kind == "peer"`: decoded message body with the peer envelope stripped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// `kind == "peer"`: kernel-verified pid of the connecting process.
    #[serde(
        rename = "verifiedPeerPid",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub verified_peer_pid: Option<i64>,
    /// `kind == "task-notification"`: scheduled trigger or peer message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subkind: Option<TaskNotificationOriginSubkind>,
    /// Keys not modelled above, passed through verbatim.
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl MessageOrigin {
    /// Build an origin from a raw wire value, mirroring Python's
    /// `_parse_origin`: anything that is not an object with a string `kind` is
    /// `None`. Mistyped optional keys are kept in `extra` rather than failing.
    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        let map = value.as_object()?;
        let kind = map.get("kind")?.as_str()?;
        if let Ok(origin) = serde_json::from_value::<MessageOrigin>(value.clone()) {
            return Some(origin);
        }
        let mut extra = map.clone();
        extra.remove("kind");
        Some(MessageOrigin {
            kind: MessageOriginKind::from(kind),
            server: None,
            from_: None,
            name: None,
            from_session: None,
            sender_task_id: None,
            body: None,
            verified_peer_pid: None,
            subkind: None,
            extra,
        })
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Emitted when the session's conversation is replaced without ending the
/// connection (e.g. after `/clear`).
///
/// A reset clears the conversation history *and* zeroes the running totals
/// reported on subsequent result messages (e.g. `total_cost_usd`); snapshot
/// accumulated totals when this arrives. `session_id` is the *outgoing*
/// session: messages after the reset carry a new `session_id`.
/// `new_conversation_id` is an opaque id for the fresh conversation, not the
/// next `session_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationResetMessage {
    pub new_conversation_id: String,
    pub uuid: String,
    pub session_id: String,
}

#[allow(clippy::large_enum_variant)] // wire-shaped enum; variants are moved, not copied
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    #[serde(rename = "user")]
    UserMsg {
        #[serde(rename = "message")]
        content: UserContent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uuid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        // The CLI may serialize a tool result as either a structured object or
        // a bare string (e.g. a built-in tool's error text). Accept any JSON
        // value so message parsing never fails on a string payload.
        tool_use_result: Option<serde_json::Value>,
        /// Provenance of this message; `None` when the CLI did not attribute
        /// it. See [`MessageOrigin`].
        #[serde(
            default,
            deserialize_with = "lenient_origin",
            skip_serializing_if = "Option::is_none"
        )]
        origin: Option<MessageOrigin>,
    },
    #[serde(rename = "assistant")]
    AssistantMsg {
        #[serde(rename = "message")]
        content: AssistantContent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
        #[serde(
            default,
            deserialize_with = "lenient",
            skip_serializing_if = "Option::is_none"
        )]
        error: Option<AssistantMessageErrorKind>,
        /// Mirrors `message.usage` (populated by the parser).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<serde_json::Map<String, serde_json::Value>>,
        /// Mirrors `message.id` (populated by the parser).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        /// Mirrors `message.stop_reason` (populated by the parser).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uuid: Option<String>,
        /// `uuid` of the user message this turn is answering.
        #[serde(
            default,
            deserialize_with = "lenient",
            skip_serializing_if = "Option::is_none"
        )]
        user_message_uuid: Option<String>,
        /// `uuid` of every user message this turn has taken in so far.
        #[serde(
            default,
            deserialize_with = "lenient",
            skip_serializing_if = "Option::is_none"
        )]
        user_message_uuids: Option<Vec<String>>,
    },
    /// Generic system message. When produced by the parser, `data` is the
    /// full raw payload, including `type` and `subtype` (as in Python's
    /// `SystemMessage.data`).
    #[serde(rename = "system")]
    SystemMsg {
        subtype: String,
        #[serde(flatten, serialize_with = "serialize_system_data")]
        data: serde_json::Map<String, serde_json::Value>,
    },
    #[serde(skip_serializing, skip_deserializing)]
    TaskStartedMsg(TaskStartedMessage),
    #[serde(skip_serializing, skip_deserializing)]
    TaskProgressMsg(TaskProgressMessage),
    #[serde(skip_serializing, skip_deserializing)]
    TaskNotificationMsg(TaskNotificationMessage),
    #[serde(skip_serializing, skip_deserializing)]
    TaskUpdatedMsg(TaskUpdatedMessage),
    #[serde(skip_serializing, skip_deserializing)]
    HookEventMsg(HookEventMessage),
    #[serde(skip_serializing, skip_deserializing)]
    MirrorErrorMsg(MirrorErrorMessage),
    #[serde(rename = "result")]
    ResultMsg {
        subtype: String,
        #[serde(deserialize_with = "int_from_number")]
        duration_ms: i64,
        #[serde(deserialize_with = "int_from_number")]
        duration_api_ms: i64,
        is_error: bool,
        #[serde(deserialize_with = "int_from_number")]
        num_turns: i64,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structured_output: Option<serde_json::Value>,
        #[serde(
            default,
            deserialize_with = "falsy_as_none",
            skip_serializing_if = "Option::is_none"
        )]
        deferred_tool_use: Option<DeferredToolUse>,
        #[serde(
            default,
            deserialize_with = "lenient_errors",
            skip_serializing_if = "Option::is_none"
        )]
        errors: Option<Vec<String>>,
        #[serde(
            default,
            deserialize_with = "lenient",
            skip_serializing_if = "Option::is_none"
        )]
        api_error_status: Option<i32>,
        /// Raw `modelUsage`; see [`Message::model_usage_typed`] for the typed
        /// [`ModelUsage`] view.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[serde(rename = "modelUsage")]
        model_usage: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_denials: Option<Vec<serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uuid: Option<String>,
        /// Why the query loop terminated (e.g. `"completed"`, `"max_turns"`,
        /// `"aborted_streaming"`, `"aborted_tools"`, `"api_error"`).
        #[serde(
            default,
            deserialize_with = "lenient",
            skip_serializing_if = "Option::is_none"
        )]
        terminal_reason: Option<String>,
        /// Origin of the user message that triggered this turn.
        #[serde(
            default,
            deserialize_with = "lenient_origin",
            skip_serializing_if = "Option::is_none"
        )]
        origin: Option<MessageOrigin>,
        /// `uuid` of the user message that started this turn.
        #[serde(
            default,
            deserialize_with = "lenient",
            skip_serializing_if = "Option::is_none"
        )]
        user_message_uuid: Option<String>,
        /// `uuid` of every user message this turn took in, in order.
        #[serde(
            default,
            deserialize_with = "lenient",
            skip_serializing_if = "Option::is_none"
        )]
        user_message_uuids: Option<Vec<String>>,
    },
    /// Python `StreamEvent` (the `stream_event` message). Note the crate-root
    /// `StreamEvent` is the client's own event enum, not this message.
    #[serde(rename = "stream_event")]
    StreamEventMsg {
        uuid: String,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
        /// `uuid` of the user message this turn is answering.
        #[serde(
            default,
            deserialize_with = "lenient",
            skip_serializing_if = "Option::is_none"
        )]
        user_message_uuid: Option<String>,
        /// `uuid` of every user message this turn has taken in so far.
        #[serde(
            default,
            deserialize_with = "lenient",
            skip_serializing_if = "Option::is_none"
        )]
        user_message_uuids: Option<Vec<String>>,
    },
    #[serde(rename = "rate_limit_event")]
    RateLimitEventMsg {
        rate_limit_info: RateLimitInfo,
        uuid: String,
        session_id: String,
    },
    /// Python `ConversationResetMessage` (`type: "conversation_reset"`).
    #[serde(rename = "conversation_reset")]
    ConversationResetMsg(ConversationResetMessage),
}

impl Message {
    /// The subtype and full raw payload of any system-family message,
    /// mirroring Python's `isinstance(msg, SystemMessage)` (task, hook and
    /// mirror-error messages subclass `SystemMessage` upstream).
    pub fn as_system(&self) -> Option<(&str, &JsonMap)> {
        match self {
            Message::SystemMsg { subtype, data } => Some((subtype.as_str(), data)),
            Message::TaskStartedMsg(msg) => Some((msg.subtype.as_str(), &msg.data)),
            Message::TaskProgressMsg(msg) => Some((msg.subtype.as_str(), &msg.data)),
            Message::TaskNotificationMsg(msg) => Some((msg.subtype.as_str(), &msg.data)),
            Message::TaskUpdatedMsg(msg) => Some((msg.subtype.as_str(), &msg.data)),
            Message::HookEventMsg(msg) => Some((msg.subtype.as_str(), &msg.data)),
            Message::MirrorErrorMsg(msg) => Some((
                msg.data
                    .get("subtype")
                    .and_then(|value| value.as_str())
                    .unwrap_or("mirror_error"),
                &msg.data,
            )),
            _ => None,
        }
    }

    /// Typed view of a `system`/`init` message's payload. `None` for any
    /// other message or when the payload does not fit [`SystemInitData`].
    pub fn as_system_init(&self) -> Option<SystemInitData> {
        match self {
            Message::SystemMsg { subtype, data } if subtype == "init" => {
                SystemInitData::from_data(data)
            }
            _ => None,
        }
    }

    /// Typed `modelUsage` of a result message (Python
    /// `ResultMessage.model_usage: dict[str, ModelUsage]`).
    pub fn model_usage_typed(&self) -> Option<HashMap<String, ModelUsage>> {
        match self {
            Message::ResultMsg {
                model_usage: Some(model_usage),
                ..
            } => serde_json::from_value(serde_json::Value::Object(model_usage.clone())).ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Content blocks. Unknown block types are dropped on deserialization.
    #[serde(deserialize_with = "deserialize_assistant_blocks")]
    pub content: Vec<ContentBlock>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "stop_reason")]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredToolUse {
    pub id: String,
    pub name: String,
    pub input: serde_json::Map<String, serde_json::Value>,
}

/// Per-model token usage and cost breakdown (values of `modelUsage`).
///
/// Keys match the CLI's camelCase wire shape. Parsed leniently: every field is
/// optional and unmodelled keys are kept in `extra`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search_requests: Option<i64>,
    #[serde(rename = "costUSD", skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    /// Canonical model id used for the pricing lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_model: Option<String>,
    /// API provider that served this model (`firstParty`, `bedrock`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Keys not modelled above, passed through verbatim.
    #[serde(flatten)]
    pub extra: JsonMap,
}

// ---------------------------------------------------------------------------
// system/init payload
// ---------------------------------------------------------------------------

open_string_enum! {
    /// Fast mode state reported in [`SystemInitData`].
    pub enum FastModeState {
        Off => "off",
        Cooldown => "cooldown",
        On => "on",
    }
}

open_string_enum! {
    /// Why fast mode can't serve right now, reported in [`SystemInitData`].
    pub enum FastModeDisabledReason {
        Free => "free",
        Preference => "preference",
        ExtraUsageDisabled => "extra_usage_disabled",
        NetworkError => "network_error",
        Unknown => "unknown",
        NotFirstParty => "not_first_party",
        DisabledByEnv => "disabled_by_env",
        ModelNotAllowed => "model_not_allowed",
        SdkOptInRequired => "sdk_opt_in_required",
        Pending => "pending",
    }
}

/// An MCP server listed in [`SystemInitData`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInitMcpServer {
    pub name: String,
    pub status: String,
    /// `"sdk"`, `"plugin"`, or a config scope such as `"user"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// A loaded plugin listed in [`SystemInitData`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInitPlugin {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A plugin load failure listed in [`SystemInitData`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInitPluginError {
    /// `name@marketplace`, or a positional tag such as `inline[0]`.
    pub plugin: String,
    /// Failure category from an open set (e.g. `"path-not-found"`).
    #[serde(rename = "type")]
    pub r#type: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Shape of a `system`/`init` message's payload (Python `SystemInitData`).
///
/// Wire keys are verbatim (a mix of snake_case and camelCase). The parser does
/// not validate this payload, so every field is optional; unmodelled keys are
/// kept in `extra`. Obtain it with [`Message::as_system_init`] or
/// [`SystemInitData::from_data`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInitData {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<String>>,
    #[serde(rename = "apiKeySource", skip_serializing_if = "Option::is_none")]
    pub api_key_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub betas: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_code_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<SystemInitMcpServer>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(
        rename = "permissionMode",
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_mode: Option<PermissionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slash_commands: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_slash_commands: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<SystemInitPlugin>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_errors: Option<Vec<SystemInitPluginError>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_mode_state: Option<FastModeState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_mode_disabled_reason: Option<FastModeDisabledReason>,
    /// Protocol capabilities for feature detection (open set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// Keys not modelled above, passed through verbatim.
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl SystemInitData {
    /// Parse a `system`/`init` payload. Returns `None` when it does not fit.
    pub fn from_data(data: &JsonMap) -> Option<Self> {
        serde_json::from_value(serde_json::Value::Object(data.clone())).ok()
    }
}

// ---------------------------------------------------------------------------
// Typed system messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
// Wire keys are snake_case (camelCase accepted as aliases).
pub struct TaskStartedMessage {
    /// Always `"task_started"`.
    #[serde(default)]
    pub subtype: String,
    #[serde(alias = "taskId")]
    pub task_id: String,
    pub description: String,
    pub uuid: String,
    #[serde(alias = "sessionId")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "toolUseId")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "taskType")]
    pub task_type: Option<String>,
    /// Full raw payload (including `type` and `subtype`), set by the parser.
    #[serde(skip)]
    pub data: JsonMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
// Wire keys are snake_case (camelCase accepted as aliases).
pub struct TaskProgressMessage {
    /// Always `"task_progress"`.
    #[serde(default)]
    pub subtype: String,
    #[serde(alias = "taskId")]
    pub task_id: String,
    pub description: String,
    #[serde(deserialize_with = "super::lenient_task_usage")]
    pub usage: TaskUsage,
    pub uuid: String,
    #[serde(alias = "sessionId")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "toolUseId")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "lastToolName")]
    pub last_tool_name: Option<String>,
    /// Full raw payload (including `type` and `subtype`), set by the parser.
    #[serde(skip)]
    pub data: JsonMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
// Wire keys are snake_case (camelCase accepted as aliases).
pub struct TaskNotificationMessage {
    /// Always `"task_notification"`.
    #[serde(default)]
    pub subtype: String,
    #[serde(alias = "taskId")]
    pub task_id: String,
    pub status: TaskNotificationStatus,
    #[serde(alias = "outputFile")]
    pub output_file: String,
    pub summary: String,
    pub uuid: String,
    #[serde(alias = "sessionId")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "toolUseId")]
    pub tool_use_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub usage: Option<TaskUsage>,
    /// Full raw payload (including `type` and `subtype`), set by the parser.
    #[serde(skip)]
    pub data: JsonMap,
}

/// System message emitted when a background task's state changes
/// (`system`/`task_updated`).
///
/// `patch` carries the changed fields (e.g. `status`, `end_time`); when
/// `patch.status` is terminal (see [`super::TERMINAL_TASK_STATUSES`]) the task
/// has finished. A background task's terminal state can arrive *only* as a
/// `TaskUpdatedMessage` with no accompanying `TaskNotificationMessage`, so
/// consumers tracking active task IDs should clear them on a terminal status
/// from either message.
#[derive(Debug, Clone, Serialize, Deserialize)]
// Wire keys are snake_case (camelCase accepted as aliases).
pub struct TaskUpdatedMessage {
    /// Always `"task_updated"`.
    #[serde(default)]
    pub subtype: String,
    #[serde(alias = "taskId")]
    pub task_id: String,
    pub patch: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskUpdatedStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "sessionId")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    /// Full raw payload (including `type` and `subtype`), set by the parser.
    #[serde(skip)]
    pub data: JsonMap,
}

/// Hook event emitted by the CLI when `include_hook_events` is enabled
/// (`system` messages with subtype `hook_started` / `hook_response`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookEventMessage {
    pub subtype: String,
    /// Name of the hook event (e.g. `"PreToolUse"`); `""` when the CLI sent
    /// none (first non-empty string of `hook_event`, `hook_name`,
    /// `hook_event_name`).
    #[serde(default)]
    pub hook_event_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    /// Full raw event payload, including `type` and `subtype`.
    pub data: serde_json::Map<String, serde_json::Value>,
}

/// System message emitted when a `SessionStore::append` call fails
/// (`subtype == "mirror_error"`). Non-fatal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorErrorMessage {
    /// Raw `key` object; see [`MirrorErrorMessage::session_key`] for the
    /// typed [`SessionKey`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<serde_json::Map<String, serde_json::Value>>,
    pub error: String,
    /// Full raw payload, including `type` and `subtype`.
    pub data: serde_json::Map<String, serde_json::Value>,
}

impl MirrorErrorMessage {
    /// The typed session key of the failed append (Python
    /// `MirrorErrorMessage.key: SessionKey | None`). `None` when absent or
    /// malformed.
    pub fn session_key(&self) -> Option<SessionKey> {
        let key = self.key.as_ref()?;
        serde_json::from_value(serde_json::Value::Object(key.clone())).ok()
    }
}

// ---------------------------------------------------------------------------
// User-supplied prompt input (text + images)
// ---------------------------------------------------------------------------

/// Source of an image supplied as part of a user prompt.
///
/// Serializes to the Claude Code stream-json image-source shape, matching the
/// Anthropic Messages API: a tagged `base64` source carries `media_type` plus
/// raw base64 `data`, while a `url` source references a remote image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

impl ImageSource {
    /// Parse a `data:<media_type>;base64,<data>` URL into a base64 image source.
    ///
    /// Returns `None` for inputs that are not base64-encoded `data:` URLs
    /// (e.g. remote `http(s)` URLs), which callers should route through
    /// [`ImageSource::Url`] or skip.
    pub fn from_data_url(url: &str) -> Option<Self> {
        let rest = url.strip_prefix("data:")?;
        let (meta, data) = rest.split_once(',')?;
        let meta = meta.strip_suffix(";base64")?;
        let media_type = if meta.is_empty() {
            "image/png".to_string()
        } else {
            meta.to_string()
        };
        Some(ImageSource::Base64 {
            media_type,
            data: data.to_string(),
        })
    }
}

/// A single block of user-supplied prompt content.
///
/// The Claude Code CLI accepts a user message whose `content` is either a
/// plain string or an array of these blocks; image blocks are the only way to
/// deliver real image input to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContentBlock {
    Text { text: String },
    Image { source: ImageSource },
}

impl InputContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        InputContentBlock::Text { text: text.into() }
    }

    pub fn image(source: ImageSource) -> Self {
        InputContentBlock::Image { source }
    }
}

/// User prompt content: either a plain string or a list of content blocks.
///
/// Passed to the streaming/query entrypoints. A plain string preserves the
/// historical text-only behaviour; the block form carries text and images
/// together so multimodal prompts reach the CLI as a real content array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserMessageInput {
    Text(String),
    Blocks(Vec<InputContentBlock>),
}

impl UserMessageInput {
    /// True when this input carries no text and no blocks.
    pub fn is_empty(&self) -> bool {
        match self {
            UserMessageInput::Text(text) => text.is_empty(),
            UserMessageInput::Blocks(blocks) => blocks.is_empty(),
        }
    }

    /// Render the value placed under the user message's `content` field: a JSON
    /// string for text input, or a JSON array of content blocks otherwise.
    pub fn to_content_value(&self) -> serde_json::Value {
        match self {
            UserMessageInput::Text(text) => serde_json::Value::String(text.clone()),
            UserMessageInput::Blocks(blocks) => serde_json::to_value(blocks)
                .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
        }
    }
}

impl From<String> for UserMessageInput {
    fn from(value: String) -> Self {
        UserMessageInput::Text(value)
    }
}

impl From<&str> for UserMessageInput {
    fn from(value: &str) -> Self {
        UserMessageInput::Text(value.to_string())
    }
}

impl From<Vec<InputContentBlock>> for UserMessageInput {
    fn from(blocks: Vec<InputContentBlock>) -> Self {
        UserMessageInput::Blocks(blocks)
    }
}

#[cfg(test)]
mod input_content_tests {
    use super::*;

    #[test]
    fn parses_base64_data_url() {
        let src = ImageSource::from_data_url("data:image/png;base64,QUJD").unwrap();
        assert_eq!(
            src,
            ImageSource::Base64 {
                media_type: "image/png".to_string(),
                data: "QUJD".to_string(),
            }
        );
    }

    #[test]
    fn rejects_non_base64_url() {
        assert!(ImageSource::from_data_url("https://example.com/a.png").is_none());
        assert!(ImageSource::from_data_url("data:image/png,QUJD").is_none());
    }

    #[test]
    fn text_input_serializes_to_string_content() {
        let input: UserMessageInput = "hello".into();
        assert_eq!(input.to_content_value(), serde_json::json!("hello"));
    }

    #[test]
    fn block_input_serializes_to_content_array() {
        let input = UserMessageInput::Blocks(vec![
            InputContentBlock::text("look:"),
            InputContentBlock::image(ImageSource::Base64 {
                media_type: "image/png".to_string(),
                data: "QUJD".to_string(),
            }),
        ]);
        assert_eq!(
            input.to_content_value(),
            serde_json::json!([
                {"type": "text", "text": "look:"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}}
            ])
        );
    }
}
