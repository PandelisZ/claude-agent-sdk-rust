use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::*;

#[derive(Debug, Clone)]
pub struct ClaudeAgentOptions {
    pub tools: Vec<String>,
    pub tools_set: bool,
    pub tools_preset: Option<ToolsPreset>,
    pub allowed_tools: Vec<String>,
    pub system_prompt: Option<String>,
    pub system_prompt_preset: Option<SystemPromptPreset>,
    pub system_prompt_file: Option<SystemPromptFile>,
    /// A custom system prompt that can also set `snapshot` (Python
    /// `SystemPromptCustom`). Sent as `--system-prompt <prompt>`.
    pub system_prompt_custom: Option<SystemPromptCustom>,
    pub mcp_servers: HashMap<String, MCPServerConfig>,
    pub mcp_servers_config: Option<String>,
    pub permission_mode: Option<PermissionMode>,
    pub continue_conversation: bool,
    pub resume: Option<String>,
    pub session_id: Option<String>,
    pub fork_session: bool,
    /// When resuming, only load the conversation up to and including the
    /// message with this UUID. Use with `resume` (and usually `fork_session`)
    /// to branch from an earlier point in the conversation.
    pub resume_session_at: Option<String>,
    /// With `resume_session_at`: the UUID of the user prompt whose turn this
    /// truncating resume intends to discard.
    ///
    /// The CLI validates at load time that every transcript entry after the
    /// `resume_session_at` point is attributable to that turn and refuses the
    /// resume otherwise. A refusal surfaces as an error (currently a
    /// [`crate::ProcessError`]) whose message contains
    /// `Resume rejected by --resume-drops-turn:`. Treat it as deterministic:
    /// clear the pending fork target and resume plainly rather than retrying
    /// the same request. `Some("")` is forwarded as-is so the CLI rejects it.
    pub resume_drops_turn: Option<String>,
    pub max_turns: Option<i32>,
    pub max_budget_usd: Option<f64>,
    pub task_budget: Option<TaskBudget>,
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
    /// Arbitrary extra CLI flags, emitted in insertion order as `--flag`
    /// (value `None`), `--flag=value` (value starting with `-`), or
    /// `--flag value`.
    pub extra_args: Vec<(String, Option<String>)>,
    pub max_buffer_size: Option<usize>,
    pub user: Option<String>,
    pub include_partial_messages: bool,
    pub include_hook_events: bool,
    /// Forward subagent text and thinking blocks as messages in the stream
    /// (initialize `forwardSubagentText`).
    pub forward_subagent_text: bool,
    /// Deliver every prompt to Claude as written: every outgoing user message
    /// is marked `client_composed: true`, so Claude Code does no `@path`
    /// expansion or slash-command dispatch. Requires Claude Code 2.1.248+.
    pub verbatim_prompts: bool,
    pub strict_mcp_config: bool,
    pub setting_sources: Option<Vec<SettingSource>>,
    pub skills: Option<SkillsConfig>,
    pub sandbox: Option<SandboxSettings>,
    pub plugins: Vec<SDKPluginConfig>,
    pub max_thinking_tokens: Option<i32>,
    pub thinking: Option<ThinkingConfig>,
    pub effort: Option<EffortLevel>,
    pub output_format: Option<serde_json::Map<String, serde_json::Value>>,
    pub enable_file_checkpointing: bool,
    pub stderr: Option<StderrCallback>,
    pub can_use_tool: Option<CanUseToolCallback>,
    pub sdk_mcp_servers: HashMap<String, crate::mcp::SimpleMCPServer>,
    pub hooks: HookMap,
    pub agents: HashMap<String, AgentDefinition>,
    pub session_store: Option<crate::session_store::SessionStoreHandle>,
    pub session_store_flush: SessionStoreFlushMode,
    pub load_timeout_ms: i32,
    /// Your app/library identifier (e.g. `"my-app/1.0.0"`), sent on requests
    /// to the API via `CLAUDE_AGENT_SDK_CLIENT_APP`. The value must be valid
    /// in an HTTP header: a character above U+00FF stops every request.
    /// `Some("")` sends no identifier (opt-out) and wins over `env`.
    pub client_app: Option<String>,
}

impl Default for ClaudeAgentOptions {
    fn default() -> Self {
        Self {
            tools: Vec::new(),
            tools_set: false,
            tools_preset: None,
            allowed_tools: Vec::new(),
            system_prompt: None,
            system_prompt_preset: None,
            system_prompt_file: None,
            system_prompt_custom: None,
            mcp_servers: HashMap::new(),
            mcp_servers_config: None,
            permission_mode: None,
            continue_conversation: false,
            resume: None,
            session_id: None,
            fork_session: false,
            resume_session_at: None,
            resume_drops_turn: None,
            max_turns: None,
            max_budget_usd: None,
            task_budget: None,
            disallowed_tools: Vec::new(),
            model: None,
            fallback_model: None,
            betas: Vec::new(),
            permission_prompt_tool_name: None,
            cwd: None,
            cli_path: None,
            settings: None,
            add_dirs: Vec::new(),
            env: HashMap::new(),
            extra_args: Vec::new(),
            max_buffer_size: None,
            user: None,
            include_partial_messages: false,
            include_hook_events: false,
            forward_subagent_text: false,
            verbatim_prompts: false,
            strict_mcp_config: false,
            setting_sources: None,
            skills: None,
            sandbox: None,
            plugins: Vec::new(),
            max_thinking_tokens: None,
            thinking: None,
            effort: None,
            output_format: None,
            enable_file_checkpointing: false,
            stderr: None,
            can_use_tool: None,
            sdk_mcp_servers: HashMap::new(),
            hooks: HashMap::new(),
            agents: HashMap::new(),
            session_store: None,
            session_store_flush: SessionStoreFlushMode::Batched,
            load_timeout_ms: 60_000,
            client_app: None,
        }
    }
}

/// A custom system prompt, in the form that can also set `snapshot`
/// (Python `SystemPromptCustom`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemPromptCustom {
    /// Always `"custom"`.
    pub r#type: String,
    pub prompt: String,
    /// Same as `SystemPromptPreset.snapshot`, applied to `prompt`: whether the
    /// session keeps the system prompt it recorded on its first request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<bool>,
}

impl SystemPromptCustom {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            r#type: "custom".to_string(),
            prompt: prompt.into(),
            snapshot: None,
        }
    }

    pub fn with_snapshot(mut self, snapshot: bool) -> Self {
        self.snapshot = Some(snapshot);
        self
    }
}

/// The Python `system_prompt` union: `str | SystemPromptPreset |
/// SystemPromptCustom | SystemPromptFile`.
///
/// `ClaudeAgentOptions` stores each form in its own field; the builder setters
/// keep them mutually exclusive (the last one set wins, as with the Python
/// union). When several fields are set directly, the precedence is
/// file > custom > preset > text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Text(String),
    Preset(SystemPromptPreset),
    Custom(SystemPromptCustom),
    File(SystemPromptFile),
}

impl From<String> for SystemPrompt {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for SystemPrompt {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<SystemPromptPreset> for SystemPrompt {
    fn from(value: SystemPromptPreset) -> Self {
        Self::Preset(value)
    }
}

impl From<SystemPromptCustom> for SystemPrompt {
    fn from(value: SystemPromptCustom) -> Self {
        Self::Custom(value)
    }
}

impl From<SystemPromptFile> for SystemPrompt {
    fn from(value: SystemPromptFile) -> Self {
        Self::File(value)
    }
}

impl ClaudeAgentOptions {
    /// Replace the system prompt with `prompt`, clearing every other form.
    pub fn set_system_prompt(&mut self, prompt: impl Into<SystemPrompt>) {
        self.system_prompt = None;
        self.system_prompt_preset = None;
        self.system_prompt_custom = None;
        self.system_prompt_file = None;
        match prompt.into() {
            SystemPrompt::Text(text) => self.system_prompt = Some(text),
            SystemPrompt::Preset(preset) => self.system_prompt_preset = Some(preset),
            SystemPrompt::Custom(custom) => self.system_prompt_custom = Some(custom),
            SystemPrompt::File(file) => self.system_prompt_file = Some(file),
        }
    }

    /// The system prompt in effect (file > custom > preset > text), or `None`
    /// when no system prompt is configured (the CLI then gets
    /// `--system-prompt ""`).
    pub fn active_system_prompt(&self) -> Option<SystemPrompt> {
        if let Some(file) = &self.system_prompt_file {
            return Some(SystemPrompt::File(file.clone()));
        }
        if let Some(custom) = &self.system_prompt_custom {
            return Some(SystemPrompt::Custom(custom.clone()));
        }
        if let Some(preset) = &self.system_prompt_preset {
            return Some(SystemPrompt::Preset(preset.clone()));
        }
        self.system_prompt.clone().map(SystemPrompt::Text)
    }

    /// The initialize request's `excludeDynamicSections`: only sent when the
    /// active system prompt is a preset that sets it.
    pub fn exclude_dynamic_sections(&self) -> Option<bool> {
        match self.active_system_prompt()? {
            SystemPrompt::Preset(preset) => preset.exclude_dynamic_sections,
            _ => None,
        }
    }

    /// The initialize request's `systemPromptSnapshot`: only sent when the
    /// active system prompt is a preset or custom prompt that sets
    /// `snapshot`.
    pub fn system_prompt_snapshot(&self) -> Option<bool> {
        match self.active_system_prompt()? {
            SystemPrompt::Custom(custom) => custom.snapshot,
            // Read through serde so this picks up `SystemPromptPreset.snapshot`
            // without naming the field here.
            SystemPrompt::Preset(preset) => serde_json::to_value(&preset)
                .ok()?
                .get("snapshot")?
                .as_bool(),
            _ => None,
        }
    }

    /// Apply `verbatim_prompts` to an outgoing user message (Python
    /// `stamp_user_message`): unchanged when the option is off, otherwise
    /// `client_composed` is set to `true`, overwriting any caller value.
    pub fn stamp_user_message(&self, mut message: serde_json::Value) -> serde_json::Value {
        if self.verbatim_prompts {
            if let serde_json::Value::Object(map) = &mut message {
                map.insert("client_composed".to_string(), serde_json::Value::Bool(true));
            }
        }
        message
    }
}
