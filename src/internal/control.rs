//! Control protocol payloads and handlers (Python `_internal/query.py`).
//!
//! The SDK and the CLI exchange `control_request` / `control_response`
//! frames over stdin/stdout. This module builds the payloads, answers the
//! requests the CLI sends (`can_use_tool`, `hook_callback`, `mcp_message`) and
//! provides low-level helpers that drive a single control request over a bare
//! [`Transport`]. The client and the query entry points route control frames
//! through the background runtime in [`crate::internal::runtime`] instead.

use crate::error::{CLIConnectionError, ClaudeSDKError, Result};
use crate::internal::sdk_mcp::answer_mcp_control_request;
use crate::internal::transport::Transport;
use crate::types::config::{permission_suggestions_from_values, permission_updates_to_wire};
use crate::types::hooks::{hook_matcher_wire_config, hooks_block_on_failure};
use crate::types::{
    CanUseToolCallback, ClaudeAgentOptions, HookCallback, HookContext, PermissionResult,
    SkillsConfig, ToolPermissionContext,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Everything needed to build the initialize request and answer the control
/// requests the CLI sends.
#[derive(Debug, Clone, Default)]
pub struct ControlCallbacks {
    pub can_use_tool: Option<CanUseToolCallback>,
    /// Shared (not cloned) by every concurrently running `mcp_message`
    /// handler, so duplicate-id detection and `notifications/cancelled` see
    /// one in-flight table per server.
    pub sdk_mcp_servers: Arc<HashMap<String, crate::mcp::SimpleMCPServer>>,
    pub hook_callbacks: HashMap<String, HookCallback>,
    pub hooks_config: Option<serde_json::Value>,
    pub agents: Option<serde_json::Value>,
    pub exclude_dynamic_sections: Option<bool>,
    /// `systemPromptSnapshot` (preset or custom system prompt `snapshot`).
    pub system_prompt_snapshot: Option<bool>,
    pub skills: Option<Vec<String>>,
    /// Send `forwardSubagentText: true` on initialize.
    pub forward_subagent_text: bool,
    /// Some hook matcher fails closed (`on_failure` other than `"continue"`),
    /// so the initialize response must confirm the CLI honors it.
    pub hooks_block_on_failure: bool,
}

impl ControlCallbacks {
    pub fn from_options(options: &ClaudeAgentOptions) -> Self {
        let (hooks_config, hook_callbacks) = build_hooks_config(options);
        Self {
            can_use_tool: options.can_use_tool.clone(),
            sdk_mcp_servers: Arc::new(options.sdk_mcp_servers.clone()),
            hook_callbacks,
            hooks_config,
            agents: agents_config(options),
            exclude_dynamic_sections: options.exclude_dynamic_sections(),
            system_prompt_snapshot: options.system_prompt_snapshot(),
            skills: match &options.skills {
                Some(SkillsConfig::Names(skills)) => Some(skills.clone()),
                Some(SkillsConfig::All) | None => None,
            },
            forward_subagent_text: options.forward_subagent_text,
            hooks_block_on_failure: hooks_block_on_failure(&options.hooks),
        }
    }
}

/// The `initialize` control request body (Python `Query.initialize`).
pub fn initialize_request(callbacks: &ControlCallbacks) -> serde_json::Value {
    let mut request = serde_json::Map::new();
    request.insert(
        "subtype".to_string(),
        serde_json::Value::String("initialize".to_string()),
    );
    request.insert(
        "hooks".to_string(),
        callbacks
            .hooks_config
            .clone()
            .unwrap_or(serde_json::Value::Null),
    );
    if let Some(agents) = &callbacks.agents {
        request.insert("agents".to_string(), agents.clone());
    }
    if let Some(exclude_dynamic_sections) = callbacks.exclude_dynamic_sections {
        request.insert(
            "excludeDynamicSections".to_string(),
            serde_json::Value::Bool(exclude_dynamic_sections),
        );
    }
    if let Some(snapshot) = callbacks.system_prompt_snapshot {
        request.insert(
            "systemPromptSnapshot".to_string(),
            serde_json::Value::Bool(snapshot),
        );
    }
    if let Some(skills) = &callbacks.skills {
        request.insert("skills".to_string(), serde_json::json!(skills));
    }
    if callbacks.forward_subagent_text {
        request.insert(
            "forwardSubagentText".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    serde_json::Value::Object(request)
}

fn agents_config(options: &ClaudeAgentOptions) -> Option<serde_json::Value> {
    if options.agents.is_empty() {
        return None;
    }

    let mut agents = serde_json::Map::new();
    let mut names: Vec<_> = options.agents.keys().cloned().collect();
    names.sort();
    for name in names {
        let Some(agent) = options.agents.get(&name) else {
            continue;
        };
        agents.insert(name, serde_json::to_value(agent).ok()?);
    }
    Some(serde_json::Value::Object(agents))
}

pub fn control_request_payload(request_id: &str, request: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    })
}

pub fn control_error_response_payload(request_id: &str, error: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": request_id,
            "error": error,
        },
    })
}

pub fn control_success_response_payload(
    request_id: &str,
    response: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
}

/// Drive one control request over a bare transport, answering any control
/// request the CLI sends meanwhile. Other frames read while waiting are
/// discarded; sessions use the background runtime instead.
pub async fn send_control_request(
    transport: &mut dyn Transport,
    request: serde_json::Value,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    send_control_request_with_callbacks(transport, request, &ControlCallbacks::default()).await
}

/// [`send_control_request`] with callbacks for the CLI's own requests.
pub async fn send_control_request_with_callbacks(
    transport: &mut dyn Transport,
    request: serde_json::Value,
    callbacks: &ControlCallbacks,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    send_control_request_with_callbacks_and_timeout(
        transport,
        request,
        callbacks,
        Duration::from_secs(60),
    )
    .await
}

pub(crate) async fn send_control_request_with_callbacks_and_timeout(
    transport: &mut dyn Transport,
    request: serde_json::Value,
    callbacks: &ControlCallbacks,
    timeout_duration: Duration,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let request_id = new_request_id(1);
    let subtype = request_subtype(&request);
    match tokio::time::timeout(
        timeout_duration,
        send_control_request_with_id(transport, &request_id, request, callbacks),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(control_timeout_error(&subtype)),
    }
}

/// `req_{counter}_{8 hex}` (Python `f"req_{n}_{os.urandom(4).hex()}"`).
pub(crate) fn new_request_id(counter: u64) -> String {
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!("req_{counter}_{}", &random[..8])
}

pub(crate) fn request_subtype(request: &serde_json::Value) -> String {
    request
        .get("subtype")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

pub(crate) fn control_timeout_error(subtype: &str) -> ClaudeSDKError {
    ClaudeSDKError::Other(format!("Control request timeout: {subtype}"))
}

pub(crate) fn initialize_timeout_duration() -> Duration {
    initialize_timeout_from_millis_env_value(
        std::env::var("CLAUDE_CODE_STREAM_CLOSE_TIMEOUT")
            .ok()
            .as_deref(),
    )
}

fn initialize_timeout_from_millis_env_value(value: Option<&str>) -> Duration {
    let millis = value
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60_000)
        .max(60_000);
    Duration::from_millis(millis)
}

async fn send_control_request_with_id(
    transport: &mut dyn Transport,
    request_id: &str,
    request: serde_json::Value,
    callbacks: &ControlCallbacks,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let subtype = request_subtype(&request);
    let payload = control_request_payload(request_id, request);
    let mut encoded = serde_json::to_vec(&payload)?;
    encoded.push(b'\n');
    transport.write(&encoded).await?;

    while let Some(data) = transport.read().await? {
        let value: serde_json::Value = serde_json::from_slice(&data)?;
        match value.get("type").and_then(|v| v.as_str()) {
            Some("control_response") => {
                if let Some(response) = matching_control_response(&value, request_id) {
                    return parse_control_response(response, &subtype);
                }
            }
            Some("control_request") => {
                respond_to_control_request(transport, &value, callbacks).await?;
            }
            _ => {}
        }
    }

    Err(CLIConnectionError::new(format!("control request ended before response: {subtype}")).into())
}

fn matching_control_response<'a>(
    value: &'a serde_json::Value,
    request_id: &str,
) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
    let response = value.get("response")?.as_object()?;
    let response_id = response.get("request_id")?.as_str()?;
    (response_id == request_id).then_some(response)
}

/// Turn a `control_response.response` object into the request's result:
/// `subtype: "error"` fails with its `error` text, anything else succeeds
/// with the inner `response` object (`{}` when absent or not an object).
pub(crate) fn parse_control_response(
    response: &serde_json::Map<String, serde_json::Value>,
    subtype: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    if response.get("subtype").and_then(|v| v.as_str()) == Some("error") {
        return Err(ClaudeSDKError::ControlRequest {
            subtype: subtype.to_string(),
            message: response
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string(),
        });
    }
    Ok(response
        .get("response")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default())
}

/// Answer a control request inline on `transport`.
pub(crate) async fn respond_to_control_request(
    transport: &mut dyn Transport,
    value: &serde_json::Value,
    callbacks: &ControlCallbacks,
) -> Result<()> {
    let Some(response) = control_request_response(callbacks, value).await else {
        return Ok(());
    };
    let mut encoded = serde_json::to_vec(&response)?;
    encoded.push(b'\n');
    transport.write(&encoded).await
}

/// Build the `control_response` frame for one incoming `control_request`
/// (Python `Query._handle_control_request`). `None` when the frame carries
/// no `request_id` to answer.
pub async fn control_request_response(
    callbacks: &ControlCallbacks,
    value: &serde_json::Value,
) -> Option<serde_json::Value> {
    let request_id = value.get("request_id").and_then(|v| v.as_str())?;
    let request = value
        .get("request")
        .and_then(|request| request.as_object())
        .cloned()
        .unwrap_or_default();
    let subtype = request
        .get("subtype")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let result: std::result::Result<serde_json::Value, String> = match subtype {
        "can_use_tool" => answer_can_use_tool(&request, callbacks)
            .await
            .map_err(|error| control_error_text(&error)),
        "hook_callback" => answer_hook_callback(&request, callbacks)
            .await
            .map_err(|error| control_error_text(&error)),
        "mcp_message" => answer_mcp_control_request(&callbacks.sdk_mcp_servers, &request).await,
        other => Err(format!("Unsupported control request subtype: {other}")),
    };
    Some(match result {
        Ok(response) => control_success_response_payload(request_id, response),
        Err(error) => control_error_response_payload(request_id, &error),
    })
}

/// The text sent in a control error response: the error's own message
/// (Python `str(e)`), without the SDK's category prefix.
pub(crate) fn control_error_text(error: &ClaudeSDKError) -> String {
    match error {
        ClaudeSDKError::ControlRequest { message, .. } => message.clone(),
        ClaudeSDKError::MCP(message)
        | ClaudeSDKError::Session(message)
        | ClaudeSDKError::Other(message) => message.clone(),
        ClaudeSDKError::IO(error) => error.to_string(),
        ClaudeSDKError::Serialization(error) => error.to_string(),
        other => other.to_string(),
    }
}

/// Build the initialize `hooks` config: one entry per event with at least
/// one matcher (Python skips empty lists), `None` when no event remains.
fn build_hooks_config(
    options: &ClaudeAgentOptions,
) -> (Option<serde_json::Value>, HashMap<String, HookCallback>) {
    let mut callback_index = 0usize;
    let mut hook_callbacks = HashMap::new();
    let mut config = serde_json::Map::new();
    let mut events: Vec<_> = options.hooks.keys().cloned().collect();
    events.sort();

    for event in events {
        let Some(matchers) = options.hooks.get(&event) else {
            continue;
        };
        if matchers.is_empty() {
            continue;
        }
        let mut matcher_values = Vec::new();
        for matcher in matchers {
            let mut callback_ids = Vec::new();
            for callback in &matcher.hooks {
                let callback_id = format!("hook_{callback_index}");
                callback_index += 1;
                hook_callbacks.insert(callback_id.clone(), callback.clone());
                callback_ids.push(callback_id);
            }
            matcher_values.push(hook_matcher_wire_config(matcher, callback_ids));
        }
        config.insert(event, serde_json::Value::Array(matcher_values));
    }

    if config.is_empty() {
        return (None, hook_callbacks);
    }
    (Some(serde_json::Value::Object(config)), hook_callbacks)
}

async fn answer_hook_callback(
    request: &serde_json::Map<String, serde_json::Value>,
    callbacks: &ControlCallbacks,
) -> Result<serde_json::Value> {
    let callback_id =
        string_field(request, "callback_id").ok_or_else(|| ClaudeSDKError::ControlRequest {
            subtype: "hook_callback".to_string(),
            message: "missing callback_id".to_string(),
        })?;
    let callback = callbacks.hook_callbacks.get(&callback_id).ok_or_else(|| {
        ClaudeSDKError::ControlRequest {
            subtype: "hook_callback".to_string(),
            message: format!("No hook callback found for ID: {callback_id}"),
        }
    })?;
    let input = request
        .get("input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let tool_use_id = string_field(request, "tool_use_id");
    let output = callback
        .call(input, tool_use_id, HookContext::default())
        .await?;
    convert_hook_output_for_cli(output)
}

/// Rename the Python-safe `async_` / `continue_` keys. A hook must return a
/// JSON object; anything else is a failed hook (an error response), which a
/// matcher with `on_failure: "block"` turns into a block.
fn convert_hook_output_for_cli(output: serde_json::Value) -> Result<serde_json::Value> {
    let serde_json::Value::Object(map) = output else {
        return Err(ClaudeSDKError::ControlRequest {
            subtype: "hook_callback".to_string(),
            message: "hook callback must return a JSON object".to_string(),
        });
    };
    let mut converted = serde_json::Map::new();
    for (key, value) in map {
        let key = match key.as_str() {
            "async_" => "async".to_string(),
            "continue_" => "continue".to_string(),
            _ => key,
        };
        converted.insert(key, value);
    }
    Ok(serde_json::Value::Object(converted))
}

async fn answer_can_use_tool(
    request: &serde_json::Map<String, serde_json::Value>,
    callbacks: &ControlCallbacks,
) -> Result<serde_json::Value> {
    let callback =
        callbacks
            .can_use_tool
            .as_ref()
            .ok_or_else(|| ClaudeSDKError::ControlRequest {
                subtype: "can_use_tool".to_string(),
                message: "canUseTool callback is not provided".to_string(),
            })?;
    let suggestions = permission_suggestions_from_values(
        request
            .get("permission_suggestions")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default(),
    )?;
    let tool_name = request
        .get("tool_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ClaudeSDKError::ControlRequest {
            subtype: "can_use_tool".to_string(),
            message: "missing tool_name".to_string(),
        })?
        .to_string();
    let input = request
        .get("input")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let context = ToolPermissionContext {
        suggestions,
        tool_use_id: string_field(request, "tool_use_id"),
        agent_id: string_field(request, "agent_id"),
        blocked_path: string_field(request, "blocked_path"),
        decision_reason: string_field(request, "decision_reason"),
        title: string_field(request, "title"),
        display_name: string_field(request, "display_name"),
        description: string_field(request, "description"),
    };
    let result = callback.call(tool_name, input.clone(), context).await?;
    Ok(permission_result_response(result, input))
}

fn string_field(request: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    request.get(key).and_then(|v| v.as_str()).map(String::from)
}

fn permission_result_response(
    result: PermissionResult,
    original_input: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    match result {
        PermissionResult::Allow {
            updated_input,
            updated_permissions,
        } => {
            let mut response = serde_json::Map::new();
            response.insert(
                "behavior".to_string(),
                serde_json::Value::String("allow".to_string()),
            );
            response.insert(
                "updatedInput".to_string(),
                serde_json::Value::Object(updated_input.unwrap_or(original_input)),
            );
            // All or nothing: a failed conversion drops every update (and is
            // logged), but the allow still goes through.
            if let Some(updates) = updated_permissions
                .as_deref()
                .and_then(permission_updates_to_wire)
            {
                response.insert(
                    "updatedPermissions".to_string(),
                    serde_json::Value::Array(updates),
                );
            }
            serde_json::Value::Object(response)
        }
        PermissionResult::Deny { message, interrupt } => {
            let mut response = serde_json::Map::new();
            response.insert(
                "behavior".to_string(),
                serde_json::Value::String("deny".to_string()),
            );
            response.insert("message".to_string(), serde_json::Value::String(message));
            if interrupt {
                response.insert("interrupt".to_string(), serde_json::Value::Bool(true));
            }
            serde_json::Value::Object(response)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_timeout_defaults_to_sixty_seconds() {
        assert_eq!(
            initialize_timeout_from_millis_env_value(None),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn initialize_timeout_uses_env_millis_when_above_minimum() {
        assert_eq!(
            initialize_timeout_from_millis_env_value(Some("120000")),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn initialize_timeout_keeps_sixty_second_minimum() {
        assert_eq!(
            initialize_timeout_from_millis_env_value(Some("1000")),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn request_ids_follow_python_shape() {
        let id = new_request_id(7);
        let (prefix, random) = id.rsplit_once('_').unwrap();
        assert_eq!(prefix, "req_7");
        assert_eq!(random.len(), 8);
        assert!(random.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn control_error_text_drops_sdk_prefixes() {
        assert_eq!(
            control_error_text(&ClaudeSDKError::ControlRequest {
                subtype: "hook_callback".into(),
                message: "No hook callback found for ID: hook_3".into(),
            }),
            "No hook callback found for ID: hook_3"
        );
        assert_eq!(
            control_error_text(&ClaudeSDKError::MCP("boom".into())),
            "boom"
        );
        assert_eq!(
            control_error_text(&CLIConnectionError::new("gone").into()),
            "gone"
        );
    }

    #[test]
    fn hook_output_must_be_an_object() {
        assert!(convert_hook_output_for_cli(serde_json::Value::Null).is_err());
        assert!(convert_hook_output_for_cli(serde_json::json!([1])).is_err());
        assert_eq!(
            convert_hook_output_for_cli(serde_json::json!({"async_": true, "continue_": false}))
                .unwrap(),
            serde_json::json!({"async": true, "continue": false})
        );
    }
}
