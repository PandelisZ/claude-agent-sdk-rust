use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Enums and String Constants

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SdkBeta {
    #[serde(rename = "context-1m-2025-08-07")]
    Context1M20250807,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingSource {
    User,
    Project,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThinkingConfigType {
    Adaptive,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserContentKind {
    Text,
    Blocks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessageErrorKind {
    AuthenticationFailed,
    BillingError,
    RateLimit,
    InvalidRequest,
    ServerError,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskNotificationStatus {
    Completed,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RateLimitStatus {
    Allowed,
    AllowedWarning,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitType {
    FiveHour,
    SevenDay,
    SevenDayOpus,
    SevenDaySonnet,
    Overage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MCPServerConnectionStatus {
    Connected,
    Failed,
    #[serde(rename = "needs-auth")]
    NeedsAuth,
    Pending,
    Disabled,
}

// MCP Server Config Types (tagged enum pattern)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MCPServerConfig {
    #[serde(rename = "stdio")]
    Stdio {
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<HashMap<String, String>>,
    },
    Sse {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        headers: Option<HashMap<String, String>>,
    },
    Http {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        headers: Option<HashMap<String, String>>,
    },
    #[serde(rename = "sdk")]
    Sdk {
        name: String,
    },
}

// Simple Struct Types

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsPreset {
    pub r#type: String,
    pub preset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemPromptPreset {
    pub r#type: String,
    pub preset: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemPromptFile {
    pub r#type: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    pub r#type: ThinkingConfigType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SDKPluginConfig {
    pub r#type: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_world: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPToolInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<MCPToolAnnotations>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPServerStatus {
    pub name: String,
    pub status: MCPServerConnectionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_info: Option<MCPServerInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<MCPServerStatusConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<MCPToolInfo>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPStatusResponse {
    pub mcp_servers: Vec<MCPServerStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskUsage {
    pub total_tokens: i32,
    pub tool_uses: i32,
    pub duration_ms: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitInfo {
    pub status: RateLimitStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_type: Option<RateLimitType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utilization: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overage_status: Option<RateLimitStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overage_resets_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overage_disabled_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Map<String, serde_json::Value>>,
}

// MCP Server Status Config (tagged enum)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MCPServerStatusConfig {
    #[serde(rename = "sdk")]
    Sdk {
        name: String,
    },
    #[serde(rename = "claudeai-proxy")]
    ClaudeAiProxy {
        url: String,
        id: String,
    },
    Sse {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        headers: Option<HashMap<String, String>>,
    },
    Http {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        headers: Option<HashMap<String, String>>,
    },
    #[serde(rename = "stdio")]
    Stdio {
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<HashMap<String, String>>,
    },
}

// Content Block Types (tagged enum)

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
}

// User Content Type

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserContent {
    pub kind: UserContentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<ContentBlock>>,
}

// Message Types (tagged enum - the main dispatch type)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
        tool_use_result: Option<serde_json::Map<String, serde_json::Value>>,
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
    pub content: Vec<ContentBlock>,
    pub model: String,
}

// Typed System Message Subtypes

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStartedMessage {
    pub task_id: String,
    pub description: String,
    pub uuid: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgressMessage {
    pub task_id: String,
    pub description: String,
    pub usage: TaskUsage,
    pub uuid: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tool_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskNotificationMessage {
    pub task_id: String,
    pub status: TaskNotificationStatus,
    pub output_file: String,
    pub summary: String,
    pub uuid: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TaskUsage>,
}

// ClaudeAgentOptions

#[derive(Debug, Clone, Default)]
pub struct ClaudeAgentOptions {
    pub tools: Vec<String>,
    pub tools_preset: Option<ToolsPreset>,
    pub allowed_tools: Vec<String>,
    pub system_prompt: Option<String>,
    pub system_prompt_preset: Option<SystemPromptPreset>,
    pub system_prompt_file: Option<SystemPromptFile>,
    pub mcp_servers: HashMap<String, MCPServerConfig>,
    pub permission_mode: Option<PermissionMode>,
    pub continue_conversation: bool,
    pub resume: Option<String>,
    pub fork_session: bool,
    pub max_turns: Option<i32>,
    pub max_budget_usd: Option<f64>,
    pub disallowed_tools: Vec<String>,
    pub model: Option<String>,
    pub fallback_model: Option<String>,
    pub betas: Vec<SdkBeta>,
    pub permission_prompt_tool_name: Option<String>,
    pub cwd: Option<String>,
    pub cli_path: Option<String>,
    pub settings: Option<String>,
    pub add_dirs: Vec<String>,
    pub env: HashMap<String, String>,
    pub extra_args: HashMap<String, Option<String>>,
    pub max_buffer_size: Option<usize>,
    pub user: Option<String>,
    pub include_partial_messages: bool,
    pub setting_sources: Vec<SettingSource>,
    pub plugins: Vec<SDKPluginConfig>,
    pub max_thinking_tokens: Option<i32>,
    pub thinking: Option<ThinkingConfig>,
    pub effort: Option<String>,
    pub output_format: Option<serde_json::Map<String, serde_json::Value>>,
    pub enable_file_checkpointing: bool,
}
