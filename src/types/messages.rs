use serde::{Deserialize, Serialize};

use super::{AssistantMessageErrorKind, RateLimitInfo, TaskNotificationStatus, TaskUsage};

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
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    ServerToolUse {
        id: String,
        name: String,
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
    pub content: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    #[serde(rename = "user")]
    UserMsg {
        #[serde(rename = "message")]
        content: UserContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        uuid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        // The CLI may serialize a tool result as either a structured object or
        // a bare string (e.g. a built-in tool's error text). Accept any JSON
        // value so message parsing never fails on a string payload.
        tool_use_result: Option<serde_json::Value>,
    },
    #[serde(rename = "assistant")]
    AssistantMsg {
        #[serde(rename = "message")]
        content: AssistantContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<AssistantMessageErrorKind>,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        uuid: Option<String>,
    },
    #[serde(rename = "system")]
    SystemMsg {
        subtype: String,
        #[serde(flatten)]
        data: serde_json::Map<String, serde_json::Value>,
    },
    #[serde(skip_serializing, skip_deserializing)]
    TaskStartedMsg(TaskStartedMessage),
    #[serde(skip_serializing, skip_deserializing)]
    TaskProgressMsg(TaskProgressMessage),
    #[serde(skip_serializing, skip_deserializing)]
    TaskNotificationMsg(TaskNotificationMessage),
    #[serde(skip_serializing, skip_deserializing)]
    HookEventMsg(HookEventMessage),
    #[serde(skip_serializing, skip_deserializing)]
    MirrorErrorMsg(MirrorErrorMessage),
    #[serde(rename = "result")]
    ResultMsg {
        subtype: String,
        duration_ms: i32,
        duration_api_ms: i32,
        is_error: bool,
        num_turns: i32,
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total_cost_usd: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        structured_output: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        deferred_tool_use: Option<DeferredToolUse>,
        #[serde(skip_serializing_if = "Option::is_none")]
        errors: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        api_error_status: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "modelUsage")]
        model_usage: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        permission_denials: Option<Vec<serde_json::Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        uuid: Option<String>,
    },
    #[serde(rename = "stream_event")]
    StreamEventMsg {
        uuid: String,
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        event: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
    },
    #[serde(rename = "rate_limit_event")]
    RateLimitEventMsg {
        rate_limit_info: RateLimitInfo,
        uuid: String,
        session_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub content: Vec<ContentBlock>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStartedMessage {
    #[serde(alias = "task_id")]
    pub task_id: String,
    pub description: String,
    pub uuid: String,
    #[serde(alias = "session_id")]
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(alias = "tool_use_id")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(alias = "task_type")]
    pub task_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgressMessage {
    #[serde(alias = "task_id")]
    pub task_id: String,
    pub description: String,
    pub usage: TaskUsage,
    pub uuid: String,
    #[serde(alias = "session_id")]
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(alias = "tool_use_id")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(alias = "last_tool_name")]
    pub last_tool_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskNotificationMessage {
    #[serde(alias = "task_id")]
    pub task_id: String,
    pub status: TaskNotificationStatus,
    #[serde(alias = "output_file")]
    pub output_file: String,
    pub summary: String,
    pub uuid: String,
    #[serde(alias = "session_id")]
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(alias = "tool_use_id")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TaskUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookEventMessage {
    pub subtype: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_event_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub data: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorErrorMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<serde_json::Map<String, serde_json::Value>>,
    pub error: String,
    pub data: serde_json::Map<String, serde_json::Value>,
}
