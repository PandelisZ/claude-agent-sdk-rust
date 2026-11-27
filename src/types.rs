use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Defines a forward-compatible string enum: known wire values map to unit
/// variants, anything else is preserved in `Other(String)`. Serializes to the
/// bare wire string (`#[serde(from = "String", into = "String")]`).
macro_rules! open_string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident => $wire:literal $(| $alias:literal)*
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(from = "String", into = "String")]
        pub enum $name {
            $(
                $(#[$vmeta])*
                $variant,
            )+
            /// A value not known at build time (forward-compatible).
            Other(String),
        }

        impl $name {
            /// The wire string for this value.
            pub fn as_str(&self) -> &str {
                match self {
                    $( $name::$variant => $wire, )+
                    $name::Other(value) => value.as_str(),
                }
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                match value.as_str() {
                    $( $wire $(| $alias)* => $name::$variant, )+
                    _ => $name::Other(value),
                }
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                $name::from(value.to_string())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                match value {
                    $name::Other(value) => value,
                    other => other.as_str().to_string(),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

pub mod messages;
pub use messages::*;
pub mod hooks;
pub use hooks::*;
pub mod config;
pub use config::*;
pub mod agent_options;
pub use agent_options::*;

// Enums and String Constants

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
    DontAsk,
    Auto,
}

impl PermissionMode {
    /// The wire string for this mode (`"default"`, `"acceptEdits"`, ...).
    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Plan => "plan",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Auto => "auto",
        }
    }

    /// Parse a wire string; `None` for a mode this SDK version does not know.
    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "default" => PermissionMode::Default,
            "acceptEdits" => PermissionMode::AcceptEdits,
            "plan" => PermissionMode::Plan,
            "bypassPermissions" => PermissionMode::BypassPermissions,
            "dontAsk" => PermissionMode::DontAsk,
            "auto" => PermissionMode::Auto,
            _ => return None,
        })
    }
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

/// How thinking content is returned (Python `ThinkingDisplay`,
/// `"summarized" | "omitted"`). Sent as `--thinking-display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingDisplay {
    Summarized,
    Omitted,
}

impl ThinkingDisplay {
    /// The bare string passed to `--thinking-display`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ThinkingDisplay::Summarized => "summarized",
            ThinkingDisplay::Omitted => "omitted",
        }
    }
}

/// Controls how much effort Claude puts into its response.
///
/// Mirrors the upstream Python `EffortLevel` literal
/// (`"low" | "medium" | "high" | "xhigh" | "max"`). Serializes to the bare
/// lowercase string the CLI expects for `--effort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl EffortLevel {
    /// The bare string value passed to the `--effort` CLI flag.
    pub fn as_cli(&self) -> &'static str {
        match self {
            EffortLevel::Low => "low",
            EffortLevel::Medium => "medium",
            EffortLevel::High => "high",
            EffortLevel::Xhigh => "xhigh",
            EffortLevel::Max => "max",
        }
    }
}

/// `AgentDefinition.effort` (Python `EffortLevel | int`): a named effort
/// level or an explicit token count. Serializes to the bare string or number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentEffort {
    Level(EffortLevel),
    Tokens(i64),
}

impl From<EffortLevel> for AgentEffort {
    fn from(value: EffortLevel) -> Self {
        AgentEffort::Level(value)
    }
}

impl From<i64> for AgentEffort {
    fn from(value: i64) -> Self {
        AgentEffort::Tokens(value)
    }
}

impl From<i32> for AgentEffort {
    fn from(value: i32) -> Self {
        AgentEffort::Tokens(value.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserContentKind {
    Text,
    Blocks,
}

open_string_enum! {
    /// Error kind reported on an assistant message (`AssistantMessage.error`).
    ///
    /// Mirrors the upstream Python `AssistantMessageError` literal. Unknown
    /// values sent by newer CLIs are preserved in
    /// [`AssistantMessageErrorKind::Other`] instead of failing the message.
    pub enum AssistantMessageErrorKind {
        AuthenticationFailed => "authentication_failed",
        BillingError => "billing_error",
        RateLimit => "rate_limit",
        InvalidRequest => "invalid_request",
        ServerError => "server_error",
        Unknown => "unknown",
    }
}

/// Python SDK-compatible name for [`AssistantMessageErrorKind`].
pub type AssistantMessageError = AssistantMessageErrorKind;

open_string_enum! {
    /// Status of a `task_notification` system message. Unknown values are
    /// preserved in [`TaskNotificationStatus::Other`].
    pub enum TaskNotificationStatus {
        Completed => "completed",
        Failed => "failed",
        Stopped => "stopped",
    }
}

open_string_enum! {
    /// Status values reported inside a `task_updated` patch.
    ///
    /// `pending`/`running`/`paused` are non-terminal; `completed`/`failed`/`killed`
    /// are terminal. Note `task_updated` reports the raw `killed`; the CLI maps that
    /// to `stopped` only when it emits a `task_notification`. Unknown values are
    /// preserved in [`TaskUpdatedStatus::Other`].
    pub enum TaskUpdatedStatus {
        Pending => "pending",
        Running => "running",
        Paused => "paused",
        Completed => "completed",
        Failed => "failed",
        Killed => "killed",
    }
}

/// Task statuses that mean the task has finished and should be cleared from any
/// "active task" tracking. Spans both lifecycle vocabularies: `task_notification`
/// reports `stopped` (the CLI's mapped form of a killed task) while `task_updated`
/// reports the raw `killed`. Consumers should treat the status of a
/// `TaskNotificationMessage` and a `TaskUpdatedMessage` the same way.
pub const TERMINAL_TASK_STATUSES: [&str; 4] = ["completed", "failed", "stopped", "killed"];

/// Returns `true` when `status` is a terminal task status (see
/// [`TERMINAL_TASK_STATUSES`]).
pub fn is_terminal_task_status(status: &str) -> bool {
    TERMINAL_TASK_STATUSES.contains(&status)
}

open_string_enum! {
    /// Rate limit status (`"allowed" | "allowed_warning" | "rejected"`).
    /// Unknown values are preserved in [`RateLimitStatus::Other`].
    pub enum RateLimitStatus {
        Allowed => "allowed",
        AllowedWarning => "allowed_warning" | "allowedWarning",
        Rejected => "rejected",
    }
}

open_string_enum! {
    /// Which rate limit window applies. Unknown values are preserved in
    /// [`RateLimitType::Other`].
    pub enum RateLimitType {
        FiveHour => "five_hour",
        SevenDay => "seven_day",
        SevenDayOpus => "seven_day_opus",
        SevenDaySonnet => "seven_day_sonnet",
        Overage => "overage",
    }
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

/// An external MCP server (Python `McpServerConfig` minus the in-process
/// `sdk` instance, which lives in `ClaudeAgentOptions::sdk_mcp_servers`).
///
/// Like Python's `McpStdioServerConfig`, `type` is optional for stdio
/// servers on input: `{"command": "..."}` deserializes as [`Self::Stdio`].
/// Serialization always writes `type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "type", rename_all = "camelCase")]
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
    Sdk { name: String },
}

impl Serialize for MCPServerConfig {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        MCPServerConfig::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for MCPServerConfig {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        if let serde_json::Value::Object(map) = &mut value {
            if !map.contains_key("type") {
                map.insert("type".to_string(), serde_json::json!("stdio"));
            }
        }
        MCPServerConfig::deserialize(value).map_err(serde::de::Error::custom)
    }
}

// Simple Struct Types

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsPreset {
    pub r#type: String,
    pub preset: String,
}

/// System prompt preset configuration (Python `SystemPromptPreset`).
///
/// Serde keys match the Python TypedDict (`exclude_dynamic_sections`, snake
/// case); the camelCase `excludeDynamicSections` is also accepted on input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemPromptPreset {
    /// Always `"preset"`.
    pub r#type: String,
    /// Always `"claude_code"`.
    pub preset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<String>,
    /// Strip per-user dynamic sections (working directory, auto-memory, git
    /// status) so the prompt stays cacheable across users. Sent as the
    /// initialize request's `excludeDynamicSections`.
    #[serde(
        default,
        alias = "excludeDynamicSections",
        skip_serializing_if = "Option::is_none"
    )]
    pub exclude_dynamic_sections: Option<bool>,
    /// Whether the session keeps the system prompt it recorded on its first
    /// request (`None` acts as `true`, except in bare mode). Sent as the
    /// initialize request's `systemPromptSnapshot`. Requires Claude Code
    /// 2.1.257+.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<bool>,
}

impl SystemPromptPreset {
    /// The `claude_code` preset with no options set.
    pub fn claude_code() -> Self {
        Self {
            r#type: "preset".to_string(),
            preset: "claude_code".to_string(),
            append: None,
            exclude_dynamic_sections: None,
            snapshot: None,
        }
    }

    pub fn with_append(mut self, append: impl Into<String>) -> Self {
        self.append = Some(append.into());
        self
    }

    pub fn with_exclude_dynamic_sections(mut self, exclude: bool) -> Self {
        self.exclude_dynamic_sections = Some(exclude);
        self
    }

    pub fn with_snapshot(mut self, snapshot: bool) -> Self {
        self.snapshot = Some(snapshot);
        self
    }
}

impl Default for SystemPromptPreset {
    fn default() -> Self {
        Self::claude_code()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemPromptFile {
    pub r#type: String,
    pub path: String,
}

/// Extended thinking configuration (the Python `ThinkingConfigAdaptive |
/// ThinkingConfigEnabled | ThinkingConfigDisabled` union, discriminated by
/// `type`).
///
/// Serde keys match the Python TypedDicts (`budget_tokens`, snake case); the
/// camelCase `budgetTokens` is also accepted on input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    pub r#type: ThinkingConfigType,
    /// Required by `enabled` (sent as `--max-thinking-tokens`).
    #[serde(
        default,
        alias = "budgetTokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub budget_tokens: Option<i32>,
    /// Ignored for `disabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ThinkingDisplay>,
}

impl ThinkingConfig {
    /// `{"type": "adaptive"}`.
    pub fn adaptive() -> Self {
        Self {
            r#type: ThinkingConfigType::Adaptive,
            budget_tokens: None,
            display: None,
        }
    }

    /// `{"type": "enabled", "budget_tokens": budget_tokens}`.
    pub fn enabled(budget_tokens: i32) -> Self {
        Self {
            r#type: ThinkingConfigType::Enabled,
            budget_tokens: Some(budget_tokens),
            display: None,
        }
    }

    /// `{"type": "disabled"}`.
    pub fn disabled() -> Self {
        Self {
            r#type: ThinkingConfigType::Disabled,
            budget_tokens: None,
            display: None,
        }
    }

    pub fn with_display(mut self, display: ThinkingDisplay) -> Self {
        self.display = Some(display);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBudget {
    pub total: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRuleValue {
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_content: Option<String>,
}

/// A permission update (Python `PermissionUpdate`).
///
/// Every enum-typed field keeps values from a newer CLI verbatim (`Other`), so
/// suggestions round-trip unchanged. Use [`PermissionUpdate::to_dict`] to
/// build the validated control-protocol form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionUpdate {
    pub r#type: PermissionUpdateType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<PermissionRuleValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<PermissionBehavior>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PermissionUpdateMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PermissionUpdateDestination>,
}

impl PermissionUpdate {
    /// An update of `kind` with every other field unset.
    pub fn new(kind: impl Into<PermissionUpdateType>) -> Self {
        Self {
            r#type: kind.into(),
            rules: None,
            behavior: None,
            mode: None,
            directories: None,
            destination: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolPermissionContext {
    pub suggestions: Vec<PermissionUpdate>,
    pub tool_use_id: Option<String>,
    pub agent_id: Option<String>,
    pub blocked_path: Option<String>,
    pub decision_reason: Option<String>,
    pub title: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PermissionResult {
    Allow {
        updated_input: Option<serde_json::Map<String, serde_json::Value>>,
        updated_permissions: Option<Vec<PermissionUpdate>>,
    },
    Deny {
        message: String,
        interrupt: bool,
    },
}

impl PermissionResult {
    pub fn allow() -> Self {
        Self::Allow {
            updated_input: None,
            updated_permissions: None,
        }
    }

    pub fn deny(message: impl Into<String>) -> Self {
        Self::Deny {
            message: message.into(),
            interrupt: false,
        }
    }
}

pub type CanUseToolFuture = Pin<Box<dyn Future<Output = Result<PermissionResult>> + Send>>;
type CanUseToolFn = dyn Fn(
        String,
        serde_json::Map<String, serde_json::Value>,
        ToolPermissionContext,
    ) -> CanUseToolFuture
    + Send
    + Sync;

#[derive(Clone)]
pub struct CanUseToolCallback(Arc<CanUseToolFn>);

impl std::fmt::Debug for CanUseToolCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CanUseToolCallback")
            .field(&"<callback>")
            .finish()
    }
}

impl CanUseToolCallback {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(String, serde_json::Map<String, serde_json::Value>, ToolPermissionContext) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<PermissionResult>> + Send + 'static,
    {
        Self(Arc::new(move |tool_name, input, context| {
            Box::pin(callback(tool_name, input, context))
        }))
    }

    pub async fn call(
        &self,
        tool_name: String,
        input: serde_json::Map<String, serde_json::Value>,
        context: ToolPermissionContext,
    ) -> Result<PermissionResult> {
        (self.0)(tool_name, input, context).await
    }
}

type StderrFn = dyn Fn(String) + Send + Sync;

#[derive(Clone)]
pub struct StderrCallback(Arc<StderrFn>);

impl std::fmt::Debug for StderrCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StderrCallback")
            .field(&"<callback>")
            .finish()
    }
}

impl StderrCallback {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        Self(Arc::new(callback))
    }

    pub fn call(&self, line: String) {
        (self.0)(line);
    }
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
pub struct ContextUsageCategory {
    pub name: String,
    pub tokens: i32,
    pub color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_deferred: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageResponse {
    pub categories: Vec<ContextUsageCategory>,
    pub total_tokens: i32,
    pub max_tokens: i32,
    pub raw_max_tokens: i32,
    pub percentage: f64,
    pub model: String,
    pub is_auto_compact_enabled: bool,
    pub memory_files: Vec<serde_json::Map<String, serde_json::Value>>,
    pub mcp_tools: Vec<serde_json::Map<String, serde_json::Value>>,
    pub agents: Vec<serde_json::Map<String, serde_json::Value>>,
    pub grid_rows: Vec<Vec<serde_json::Map<String, serde_json::Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_builtin_tools: Option<Vec<serde_json::Map<String, serde_json::Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_tools: Option<Vec<serde_json::Map<String, serde_json::Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_sections: Option<Vec<serde_json::Map<String, serde_json::Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slash_commands: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<serde_json::Value>,
}

/// Usage statistics reported in `task_progress` and `task_notification`
/// messages.
///
/// Parsed leniently (Python passes the raw dict through): missing counters
/// default to `0` and unmodelled keys are kept in `extra`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskUsage {
    #[serde(default, alias = "totalTokens", deserialize_with = "lenient_counter")]
    pub total_tokens: i64,
    #[serde(default, alias = "toolUses", deserialize_with = "lenient_counter")]
    pub tool_uses: i64,
    #[serde(default, alias = "durationMs", deserialize_with = "lenient_counter")]
    pub duration_ms: i64,
    /// Keys not modelled above, passed through verbatim.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A usage counter: integers and integral floats are kept; `null` or any
/// other shape becomes `0` instead of failing the whole message.
fn lenient_counter<'de, D>(deserializer: D) -> std::result::Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Number(number) => number.as_i64().unwrap_or_else(|| {
            number
                .as_f64()
                .filter(|float| float.is_finite() && float.fract() == 0.0)
                .map(|float| float as i64)
                .unwrap_or_default()
        }),
        _ => 0,
    })
}

/// `usage` on a task message: Python keeps the raw value; a non-object
/// becomes an all-zero [`TaskUsage`] instead of failing the message.
pub(crate) fn lenient_task_usage<'de, D>(
    deserializer: D,
) -> std::result::Result<TaskUsage, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Rate limit status emitted by the CLI when rate limit state changes.
///
/// `raw` always holds the full `rate_limit_info` object as received (it is
/// populated on deserialization and never serialized), matching Python's
/// `RateLimitInfo.raw`.
#[derive(Debug, Clone, Serialize)]
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
    /// Full raw dict from the CLI, including any fields not modelled above.
    #[serde(skip)]
    pub raw: serde_json::Map<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for RateLimitInfo {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            status: RateLimitStatus,
            #[serde(default)]
            resets_at: Option<i64>,
            #[serde(default)]
            rate_limit_type: Option<RateLimitType>,
            #[serde(default)]
            utilization: Option<f64>,
            #[serde(default)]
            overage_status: Option<RateLimitStatus>,
            #[serde(default)]
            overage_resets_at: Option<i64>,
            #[serde(default)]
            overage_disabled_reason: Option<String>,
        }

        let raw = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let wire: Wire = serde_json::from_value(serde_json::Value::Object(raw.clone()))
            .map_err(serde::de::Error::custom)?;
        Ok(RateLimitInfo {
            status: wire.status,
            resets_at: wire.resets_at,
            rate_limit_type: wire.rate_limit_type,
            utilization: wire.utilization,
            overage_status: wire.overage_status,
            overage_resets_at: wire.overage_resets_at,
            overage_disabled_reason: wire.overage_disabled_reason,
            raw,
        })
    }
}

// MCP Server Status Config (tagged enum)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MCPServerStatusConfig {
    #[serde(rename = "sdk")]
    Sdk { name: String },
    #[serde(rename = "claudeai-proxy")]
    ClaudeAiProxy { url: String, id: String },
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
