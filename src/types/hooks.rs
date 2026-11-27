use crate::error::Result;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::PermissionBehavior;

pub type HookFuture = Pin<Box<dyn Future<Output = Result<serde_json::Value>> + Send>>;
type HookFn = dyn Fn(serde_json::Value, Option<String>, HookContext) -> HookFuture + Send + Sync;

#[derive(Debug, Clone, Default)]
pub struct HookContext {}

#[derive(Clone)]
pub struct HookCallback(Arc<HookFn>);

impl std::fmt::Debug for HookCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HookCallback").field(&"<callback>").finish()
    }
}

impl HookCallback {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(serde_json::Value, Option<String>, HookContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value>> + Send + 'static,
    {
        Self(Arc::new(move |input, tool_use_id, context| {
            Box::pin(callback(input, tool_use_id, context))
        }))
    }

    pub async fn call(
        &self,
        input: serde_json::Value,
        tool_use_id: Option<String>,
        context: HookContext,
    ) -> Result<serde_json::Value> {
        (self.0)(input, tool_use_id, context).await
    }
}

/// Hook events accepted as keys of `ClaudeAgentOptions.hooks`.
///
/// Mirrors the upstream Python `HookEvent` literal. [`HookMap`] keeps `String`
/// keys for forward compatibility (the CLI also accepts events such as
/// `SessionStart` or `PostToolBatch`); `HookEvent` converts into `String`, so
/// `ClaudeAgentOptionsBuilder::hook` accepts either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    UserPromptSubmit,
    Stop,
    SubagentStop,
    PreCompact,
    Notification,
    SubagentStart,
    PermissionRequest,
}

impl HookEvent {
    /// The event name as the CLI spells it.
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::PostToolUseFailure => "PostToolUseFailure",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::Stop => "Stop",
            HookEvent::SubagentStop => "SubagentStop",
            HookEvent::PreCompact => "PreCompact",
            HookEvent::Notification => "Notification",
            HookEvent::SubagentStart => "SubagentStart",
            HookEvent::PermissionRequest => "PermissionRequest",
        }
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<HookEvent> for String {
    fn from(event: HookEvent) -> Self {
        event.as_str().to_string()
    }
}

/// What happens when a callback in a [`HookMatcher`] fails.
///
/// Mirrors the upstream Python `HookMatcher.on_failure`
/// (`Literal["continue", "block"]`). Sent on the initialize request as the
/// matcher's `onFailure` key. Unknown values are carried through verbatim in
/// [`HookOnFailure::Other`]; the CLI reads any value other than `"continue"`
/// as `"block"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookOnFailure {
    /// A failure counts as no decision (the CLI default).
    Continue,
    /// A callback that errors, returns nothing usable, or returns a value the
    /// CLI rejects blocks the action.
    Block,
    /// Any other value, passed through as-is.
    Other(String),
}

impl HookOnFailure {
    /// The wire string for this value.
    pub fn as_str(&self) -> &str {
        match self {
            HookOnFailure::Continue => "continue",
            HookOnFailure::Block => "block",
            HookOnFailure::Other(value) => value,
        }
    }

    /// Whether the CLI treats this value as fail-closed. Any value other than
    /// `"continue"` is read as `"block"`.
    pub fn blocks(&self) -> bool {
        !matches!(self, HookOnFailure::Continue)
    }
}

impl From<&str> for HookOnFailure {
    fn from(value: &str) -> Self {
        match value {
            "continue" => HookOnFailure::Continue,
            "block" => HookOnFailure::Block,
            other => HookOnFailure::Other(other.to_string()),
        }
    }
}

impl From<String> for HookOnFailure {
    fn from(value: String) -> Self {
        HookOnFailure::from(value.as_str())
    }
}

impl std::fmt::Display for HookOnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for HookOnFailure {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HookOnFailure {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Ok(HookOnFailure::from(String::deserialize(deserializer)?))
    }
}

#[derive(Debug, Clone, Default)]
pub struct HookMatcher {
    pub matcher: Option<String>,
    pub hooks: Vec<HookCallback>,
    /// Timeout in seconds for all hooks in this matcher (CLI default: 60).
    pub timeout: Option<f64>,
    /// What happens when a callback in this matcher fails. With
    /// [`HookOnFailure::Block`], a callback that errors, returns nothing, or
    /// returns a value the CLI rejects (such as `permissionDecision: "Deny"`)
    /// blocks the action, so a callback with no opinion must return `{}`.
    /// Unrecognized keys are not a failure. `Stop`, `SubagentStop`,
    /// `TaskCompleted` and `TeammateIdle` ignore this option. A
    /// `PermissionRequest` block counts only if it lands before `can_use_tool`
    /// answers. Unset or `Continue` counts a failure as no decision. With
    /// `Block`, connecting to a CLI too old to honor it fails with
    /// `CLIConnectionError`.
    pub on_failure: Option<HookOnFailure>,
}

impl HookMatcher {
    pub fn new(callback: HookCallback) -> Self {
        Self {
            matcher: None,
            hooks: vec![callback],
            timeout: None,
            on_failure: None,
        }
    }

    pub fn matcher(mut self, matcher: impl Into<String>) -> Self {
        self.matcher = Some(matcher.into());
        self
    }

    pub fn timeout(mut self, timeout: f64) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set what happens when a callback in this matcher fails (`onFailure`).
    pub fn on_failure(mut self, on_failure: impl Into<HookOnFailure>) -> Self {
        self.on_failure = Some(on_failure.into());
        self
    }

    /// Whether this matcher makes hook failures fail closed (an `on_failure`
    /// other than `"continue"`).
    pub fn blocks_on_failure(&self) -> bool {
        self.on_failure.as_ref().is_some_and(HookOnFailure::blocks)
    }
}

pub type HookMap = HashMap<String, Vec<HookMatcher>>;

/// Build the per-matcher object sent in the initialize request's `hooks`
/// config: `{"matcher", "hookCallbackIds", "timeout"?, "onFailure"?}`.
///
/// Mirrors upstream `_internal/query.py` `initialize()`: `timeout` and
/// `onFailure` are emitted only when set.
pub(crate) fn hook_matcher_wire_config(
    matcher: &HookMatcher,
    callback_ids: Vec<String>,
) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    value.insert(
        "matcher".to_string(),
        matcher
            .matcher
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    value.insert(
        "hookCallbackIds".to_string(),
        serde_json::Value::Array(
            callback_ids
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    if let Some(timeout) = matcher.timeout {
        value.insert("timeout".to_string(), serde_json::json!(timeout));
    }
    if let Some(on_failure) = &matcher.on_failure {
        value.insert(
            "onFailure".to_string(),
            serde_json::Value::String(on_failure.as_str().to_string()),
        );
    }
    serde_json::Value::Object(value)
}

/// Whether any matcher in `hooks` is set to fail closed. Mirrors upstream's
/// `blocks_on_failure` accumulator in `initialize()`.
pub(crate) fn hooks_block_on_failure(hooks: &HookMap) -> bool {
    hooks.values().flatten().any(HookMatcher::blocks_on_failure)
}

pub(crate) const HOOK_ON_FAILURE_UNSUPPORTED_MESSAGE: &str =
    "The process answering initialize does not honor on_failure on hook matchers, \
     so a hook that fails would not block. Update Claude Code if it is old, or \
     remove on_failure=\"block\" from your hooks.";

pub(crate) const HOOKS_NOT_APPLIED_MESSAGE: &str =
    "This session was already running and did not register your hooks, so a hook \
     set to on_failure=\"block\" would never run. Start a new session, or remove \
     on_failure=\"block\" from your hooks.";

/// Python truthiness of an optional JSON value.
fn json_truthy(value: Option<&serde_json::Value>) -> bool {
    match value {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        Some(serde_json::Value::Object(o)) => !o.is_empty(),
    }
}

/// Guard to run after every initialize response.
///
/// When a matcher is set to fail closed, a responder that does not honor
/// `onFailure` (no truthy `hook_on_failure_supported`) or that did not
/// register the hooks (`hooks_applied` is exactly `false`; a missing key
/// passes) would make the guard fail open or never run, so this returns a
/// `CLIConnectionError`.
pub(crate) fn check_initialize_response_hook_on_failure(
    blocks_on_failure: bool,
    response: &serde_json::Value,
) -> Result<()> {
    if !blocks_on_failure {
        return Ok(());
    }
    if !json_truthy(response.get("hook_on_failure_supported")) {
        return Err(
            crate::error::CLIConnectionError::new(HOOK_ON_FAILURE_UNSUPPORTED_MESSAGE).into(),
        );
    }
    if response.get("hooks_applied") == Some(&serde_json::Value::Bool(false)) {
        return Err(crate::error::CLIConnectionError::new(HOOKS_NOT_APPLIED_MESSAGE).into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Typed hook input/output payloads (parity with the Python SDK type surface).
//
// The hook callback itself stays generic over `serde_json::Value` for
// flexibility; these strongly-typed structs let callers deserialize a known
// event or construct a structured response, mirroring the Python TypedDicts.
// Input field names are snake_case on the wire; output field names are
// camelCase, matching the CLI exactly.
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};

/// Reasoning effort in effect when a hook fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEffort {
    /// Effort level for the current turn (e.g. `"low"`, `"high"`, `"max"`),
    /// after any downgrade for the selected model.
    pub level: String,
}

/// Base fields present across hook input events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseHookInput {
    pub session_id: String,
    pub transcript_path: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// UUID of the user prompt being processed. Absent until the first user
    /// input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    /// Effort in effect for the current turn, on models that support effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<HookEffort>,
}

/// Input data for `PreToolUse` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreToolUseHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub tool_name: String,
    pub tool_input: serde_json::Map<String, serde_json::Value>,
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// Input data for `PostToolUse` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostToolUseHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub tool_name: String,
    pub tool_input: serde_json::Map<String, serde_json::Value>,
    pub tool_response: serde_json::Value,
    pub tool_use_id: String,
    /// Tool execution time in milliseconds. Excludes permission-prompt and
    /// hook time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// Input data for `PostToolUseFailure` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostToolUseFailureHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub tool_name: String,
    pub tool_input: serde_json::Map<String, serde_json::Value>,
    pub tool_use_id: String,
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_interrupt: Option<bool>,
    /// Tool execution time in milliseconds. Excludes permission-prompt and
    /// hook time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// Input data for `UserPromptSubmit` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPromptSubmitHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub prompt: String,
}

/// In-flight background task, as listed on `Stop` and `SubagentStop` inputs.
///
/// `description` and `command` are capped at 1000 characters; a clipped value
/// ends with an in-string `"… [+N chars]"` marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTaskSummary {
    pub id: String,
    /// Task-type label (e.g. `"shell"`, `"subagent"`, `"monitor"`,
    /// `"workflow"`). Falls back to the raw discriminant for unknown types.
    pub r#type: String,
    pub status: String,
    pub description: String,
    /// Shell command line. Only present for `shell` tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Subagent type name. Only present for `subagent` tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// MCP server name. Only present for `monitor` / MCP tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// MCP tool name. Only present for `monitor` / MCP tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Workflow name. Only present for `workflow` tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Session-scoped scheduled wakeup, as listed on `Stop` and `SubagentStop`
/// inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCronSummary {
    pub id: String,
    /// Cron expression, e.g. `"0 9 * * 1-5"`.
    pub schedule: String,
    /// `false` for one-shot wakeups; `true` for tasks that re-fire on every
    /// match.
    pub recurring: bool,
    /// Prompt submitted when the cron fires (capped at 1000 characters).
    pub prompt: String,
}

/// Input data for `Stop` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub stop_hook_active: bool,
    /// Text content of the last assistant message before stopping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    /// In-flight background work in this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_tasks: Option<Vec<BackgroundTaskSummary>>,
    /// Session-scoped scheduled wakeups that will resume this session later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_crons: Option<Vec<SessionCronSummary>>,
}

/// Input data for `SubagentStop` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentStopHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub stop_hook_active: bool,
    pub agent_id: String,
    pub agent_transcript_path: String,
    pub agent_type: String,
    /// Text content of the subagent's last assistant message before stopping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    /// In-flight background work, scoped to the parent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_tasks: Option<Vec<BackgroundTaskSummary>>,
    /// Scheduled wakeups, scoped to the parent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_crons: Option<Vec<SessionCronSummary>>,
}

/// Input data for `PreCompact` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreCompactHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub trigger: String,
    pub custom_instructions: Option<String>,
}

/// Input data for `Notification` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub notification_type: String,
}

/// Input data for `SubagentStart` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentStartHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub agent_id: String,
    pub agent_type: String,
}

/// Input data for `PermissionRequest` hook events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequestHookInput {
    #[serde(flatten)]
    pub base: BaseHookInput,
    pub tool_name: String,
    pub tool_input: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_suggestions: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// Strongly-typed union of hook inputs, discriminated by `hook_event_name`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "hook_event_name")]
pub enum HookInput {
    PreToolUse(PreToolUseHookInput),
    PostToolUse(PostToolUseHookInput),
    PostToolUseFailure(PostToolUseFailureHookInput),
    UserPromptSubmit(UserPromptSubmitHookInput),
    Stop(StopHookInput),
    SubagentStop(SubagentStopHookInput),
    PreCompact(PreCompactHookInput),
    Notification(NotificationHookInput),
    SubagentStart(SubagentStartHookInput),
    PermissionRequest(PermissionRequestHookInput),
}

// ---------------------------------------------------------------------------
// Hook-specific outputs.
//
// Each `*HookSpecificOutput` struct carries the event-specific fields. The
// CLI routes a `hookSpecificOutput` by its `hookEventName` key, which the
// [`HookSpecificOutput`] enum adds as its serde tag: wrap a struct in the enum
// (every struct converts with `.into()`) before sending it.
// ---------------------------------------------------------------------------

/// `permissionDecision` values for `PreToolUse` hook output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookPermissionDecision {
    Allow,
    Deny,
    Ask,
    Defer,
}

/// `action` values for `Elicitation` / `ElicitationResult` hook output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationAction {
    Accept,
    Decline,
    Cancel,
}

/// Hook-specific output for `PreToolUse` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreToolUseHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_decision: Option<HookPermissionDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_decision_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Hook-specific output for `PostToolUse` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostToolUseHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
    /// Replaces the tool output before it is sent to the model. For built-in
    /// tools the value must match the tool's output schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_tool_output: Option<serde_json::Value>,
    /// Replaces the output for MCP tools only. Prefer `updated_tool_output`.
    #[serde(
        rename = "updatedMCPToolOutput",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_mcp_tool_output: Option<serde_json::Value>,
}

/// Hook-specific output for `PostToolUseFailure` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostToolUseFailureHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Hook-specific output for `UserPromptSubmit` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPromptSubmitHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
    /// Sets the session title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    /// When `decision` is `"block"`, omit the original prompt from the block
    /// message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_original_prompt: Option<bool>,
}

/// Hook-specific output for `UserPromptExpansion` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPromptExpansionHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_original_prompt: Option<bool>,
}

/// Hook-specific output for `SessionStart` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
    /// Used as the first user message of the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_user_message: Option<String>,
    /// Sets the session title. Ignored when the session starts from `clear`
    /// or `compact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    /// Absolute paths to watch for `FileChanged` events during this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch_paths: Option<Vec<String>>,
    /// Re-scan skill and command directories after `SessionStart` hooks
    /// complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_skills: Option<bool>,
}

/// Hook-specific output for `Notification` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Hook-specific output for `SubagentStart` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentStartHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Hook-specific output for `PermissionRequest` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequestHookSpecificOutput {
    pub decision: serde_json::Map<String, serde_json::Value>,
}

/// Hook-specific output for `PostToolBatch` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostToolBatchHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Hook-specific output for `Stop` events. `additional_context` is non-error
/// feedback delivered to the model; the conversation continues.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Hook-specific output for `SubagentStop` events. `additional_context` is
/// non-error feedback delivered to the subagent; the subagent continues.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentStopHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Hook-specific output for `PermissionDenied` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionDeniedHookSpecificOutput {
    /// Tell the model it may retry the denied tool call. Ignored for denials
    /// without a classifier verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<bool>,
}

/// Hook-specific output for `PreModelSwitch` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreModelSwitchHookSpecificOutput {
    /// `allow` proceeds, `deny` cancels the switch. `ask` is treated as a
    /// refusal outside an interactive session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_decision: Option<PermissionBehavior>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_decision_reason: Option<String>,
}

/// Hook-specific output for `PostModelSwitch` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostModelSwitchHookSpecificOutput {
    /// Reaches the model with the next request the new model serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

/// Hook-specific output for `Elicitation` events: accept or decline an MCP
/// elicitation request programmatically.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ElicitationAction>,
    /// Form field values to submit. Only used when `action` is `accept`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Hook-specific output for `ElicitationResult` events: override the action
/// or content before the response is sent to the MCP server.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationResultHookSpecificOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ElicitationAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Hook-specific output for `CwdChanged` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CwdChangedHookSpecificOutput {
    /// Absolute paths to watch for `FileChanged` events. Replaces the current
    /// dynamic watch list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch_paths: Option<Vec<String>>,
}

/// Hook-specific output for `FileChanged` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangedHookSpecificOutput {
    /// Absolute paths to watch for `FileChanged` events. Replaces the current
    /// dynamic watch list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch_paths: Option<Vec<String>>,
}

/// Hook-specific output for `WorktreeCreate` events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeCreateHookSpecificOutput {
    /// Absolute path to the created worktree directory.
    pub worktree_path: String,
}

/// Hook-specific output for `MessageDisplay` events. Display-only: replaces
/// the text on screen without changing the stored message or what the model
/// sees.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDisplayHookSpecificOutput {
    /// Text displayed in place of the delta. Omit it to display the original.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_content: Option<String>,
}

macro_rules! hook_specific_output_enum {
    ($($variant:ident($ty:ident)),* $(,)?) => {
        /// Event-specific hook output, discriminated by `hookEventName`.
        ///
        /// Mirrors the upstream Python `HookSpecificOutput` union. Serializing
        /// a variant emits `{"hookEventName": "<Variant>", ...fields}`.
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(tag = "hookEventName")]
        pub enum HookSpecificOutput {
            $($variant($ty),)*
        }

        impl HookSpecificOutput {
            /// The `hookEventName` this output is routed by.
            pub fn hook_event_name(&self) -> &'static str {
                match self {
                    $(HookSpecificOutput::$variant(_) => stringify!($variant),)*
                }
            }
        }

        $(
            impl From<$ty> for HookSpecificOutput {
                fn from(output: $ty) -> Self {
                    HookSpecificOutput::$variant(output)
                }
            }
        )*
    };
}

hook_specific_output_enum! {
    PreToolUse(PreToolUseHookSpecificOutput),
    PostToolUse(PostToolUseHookSpecificOutput),
    PostToolUseFailure(PostToolUseFailureHookSpecificOutput),
    UserPromptSubmit(UserPromptSubmitHookSpecificOutput),
    SessionStart(SessionStartHookSpecificOutput),
    Notification(NotificationHookSpecificOutput),
    SubagentStart(SubagentStartHookSpecificOutput),
    PermissionRequest(PermissionRequestHookSpecificOutput),
    UserPromptExpansion(UserPromptExpansionHookSpecificOutput),
    PostToolBatch(PostToolBatchHookSpecificOutput),
    Stop(StopHookSpecificOutput),
    SubagentStop(SubagentStopHookSpecificOutput),
    PermissionDenied(PermissionDeniedHookSpecificOutput),
    PreModelSwitch(PreModelSwitchHookSpecificOutput),
    PostModelSwitch(PostModelSwitchHookSpecificOutput),
    Elicitation(ElicitationHookSpecificOutput),
    ElicitationResult(ElicitationResultHookSpecificOutput),
    CwdChanged(CwdChangedHookSpecificOutput),
    FileChanged(FileChangedHookSpecificOutput),
    WorktreeCreate(WorktreeCreateHookSpecificOutput),
    MessageDisplay(MessageDisplayHookSpecificOutput),
}

// ---------------------------------------------------------------------------
// Hook JSON output (the value a hook callback returns).
// ---------------------------------------------------------------------------

/// `decision` values for [`SyncHookJSONOutput`]. Only `"block"` is
/// meaningful (`"approve"` is deprecated for `PreToolUse`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookDecision {
    Block,
}

/// Async hook output that defers hook execution.
///
/// Serializes `async_` as the CLI's `async` key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsyncHookJSONOutput {
    /// Must be `true` to defer hook execution (wire key `async`).
    #[serde(rename = "async")]
    pub async_: bool,
    /// Optional timeout in milliseconds for the async operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub async_timeout: Option<u64>,
}

impl Default for AsyncHookJSONOutput {
    fn default() -> Self {
        Self {
            async_: true,
            async_timeout: None,
        }
    }
}

/// Synchronous hook output with control and decision fields.
///
/// Serializes `continue_` as the CLI's `continue` key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncHookJSONOutput {
    /// Whether Claude should proceed after hook execution (CLI default: true).
    #[serde(rename = "continue", default, skip_serializing_if = "Option::is_none")]
    pub continue_: Option<bool>,
    /// Hide stdout from transcript mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_output: Option<bool>,
    /// Message shown when `continue` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<HookDecision>,
    /// Warning message displayed to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_message: Option<String>,
    /// Feedback message for Claude about the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Event-specific controls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<HookSpecificOutput>,
}

/// Typed hook callback output: [`AsyncHookJSONOutput`] or
/// [`SyncHookJSONOutput`]. Hook callbacks return `serde_json::Value`; convert
/// with `.into()` (or [`HookJSONOutput::to_value`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum HookJSONOutput {
    Async(AsyncHookJSONOutput),
    Sync(SyncHookJSONOutput),
}

impl HookJSONOutput {
    /// The CLI wire form of this output.
    pub fn to_value(&self) -> serde_json::Value {
        // Serialization of these plain data types cannot fail.
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

impl From<AsyncHookJSONOutput> for HookJSONOutput {
    fn from(output: AsyncHookJSONOutput) -> Self {
        HookJSONOutput::Async(output)
    }
}

impl From<SyncHookJSONOutput> for HookJSONOutput {
    fn from(output: SyncHookJSONOutput) -> Self {
        HookJSONOutput::Sync(output)
    }
}

impl From<HookJSONOutput> for serde_json::Value {
    fn from(output: HookJSONOutput) -> Self {
        output.to_value()
    }
}

impl From<SyncHookJSONOutput> for serde_json::Value {
    fn from(output: SyncHookJSONOutput) -> Self {
        HookJSONOutput::Sync(output).to_value()
    }
}

impl From<AsyncHookJSONOutput> for serde_json::Value {
    fn from(output: AsyncHookJSONOutput) -> Self {
        HookJSONOutput::Async(output).to_value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop() -> HookCallback {
        HookCallback::new(|_, _, _| async { Ok(serde_json::json!({})) })
    }

    #[test]
    fn matcher_wire_config_matches_python_initialize_shape() {
        let bare = HookMatcher::new(noop());
        assert_eq!(
            hook_matcher_wire_config(&bare, vec!["hook_0".into()]),
            serde_json::json!({"matcher": null, "hookCallbackIds": ["hook_0"]})
        );

        let full = HookMatcher::new(noop())
            .matcher("Bash")
            .timeout(5.0)
            .on_failure(HookOnFailure::Block);
        assert_eq!(
            hook_matcher_wire_config(&full, vec!["hook_1".into()]),
            serde_json::json!({
                "matcher": "Bash",
                "hookCallbackIds": ["hook_1"],
                "timeout": 5.0,
                "onFailure": "block"
            })
        );

        let cont = HookMatcher::new(noop()).on_failure("continue");
        assert_eq!(
            hook_matcher_wire_config(&cont, vec![])["onFailure"],
            serde_json::json!("continue")
        );
    }

    #[test]
    fn blocks_on_failure_treats_unknown_values_as_block() {
        let mut hooks = HookMap::new();
        hooks.insert("PreToolUse".into(), vec![HookMatcher::new(noop())]);
        assert!(!hooks_block_on_failure(&hooks));
        hooks.insert(
            "Stop".into(),
            vec![HookMatcher::new(noop()).on_failure("continue")],
        );
        assert!(!hooks_block_on_failure(&hooks));
        hooks.insert(
            "PostToolUse".into(),
            vec![HookMatcher::new(noop()).on_failure("weird")],
        );
        assert!(hooks_block_on_failure(&hooks));
    }

    #[test]
    fn initialize_guard_matches_python_semantics() {
        // Nothing to guard.
        assert!(check_initialize_response_hook_on_failure(false, &serde_json::json!({})).is_ok());

        let err = check_initialize_response_hook_on_failure(true, &serde_json::json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not honor on_failure"), "{err}");

        let err = check_initialize_response_hook_on_failure(
            true,
            &serde_json::json!({"hook_on_failure_supported": false}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("does not honor on_failure"), "{err}");

        let err = check_initialize_response_hook_on_failure(
            true,
            &serde_json::json!({"hook_on_failure_supported": true, "hooks_applied": false}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("already running"), "{err}");

        // Missing hooks_applied passes (strict `is False` check).
        assert!(check_initialize_response_hook_on_failure(
            true,
            &serde_json::json!({"hook_on_failure_supported": true})
        )
        .is_ok());
        assert!(check_initialize_response_hook_on_failure(
            true,
            &serde_json::json!({"hook_on_failure_supported": true, "hooks_applied": null})
        )
        .is_ok());
    }
}
