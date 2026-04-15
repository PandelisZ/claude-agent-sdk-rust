use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::error::{CLIConnectionError, CLINotFoundError, ProcessError, CLIJSONDecodeError, Result, ClaudeSDKError};
use crate::types::ClaudeAgentOptions;

const DEFAULT_CLI_NAME: &str = "claude";
const DEFAULT_ENTRY_POINT: &str = "sdk-go";
const DEFAULT_MAX_BUFFER_SIZE: usize = 1024 * 1024;

#[async_trait]
pub trait Transport: Send + Sync {
    async fn connect(&mut self) -> Result<()>;
    async fn write(&mut self, data: &[u8]) -> Result<()>;
    async fn close_input(&mut self) -> Result<()>;
    async fn read(&mut self) -> Result<Option<Vec<u8>>>;
    async fn close(&mut self) -> Result<()>;
}

#[derive(Debug)]
pub struct SubprocessCLITransport {
    options: TransportOptions,
    max_buffer_size: usize,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout_reader: Option<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
}

#[derive(Debug, Clone)]
pub struct TransportOptions {
    pub tools: Vec<String>,
    pub tools_preset: Option<crate::types::ToolsPreset>,
    pub allowed_tools: Vec<String>,
    pub system_prompt: Option<String>,
    pub system_prompt_preset: Option<crate::types::SystemPromptPreset>,
    pub system_prompt_file: Option<crate::types::SystemPromptFile>,
    pub mcp_servers: std::collections::HashMap<String, crate::types::MCPServerConfig>,
    pub permission_mode: Option<crate::types::PermissionMode>,
    pub continue_conversation: bool,
    pub resume: Option<String>,
    pub fork_session: bool,
    pub max_turns: Option<i32>,
    pub max_budget_usd: Option<f64>,
    pub disallowed_tools: Vec<String>,
    pub model: Option<String>,
    pub fallback_model: Option<String>,
    pub betas: Vec<crate::types::SdkBeta>,
    pub permission_prompt_tool_name: Option<String>,
    pub cwd: Option<String>,
    pub cli_path: Option<String>,
    pub settings: Option<String>,
    pub add_dirs: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
    pub extra_args: std::collections::HashMap<String, Option<String>>,
    pub max_buffer_size: Option<usize>,
    pub user: Option<String>,
    pub include_partial_messages: bool,
    pub setting_sources: Vec<crate::types::SettingSource>,
    pub plugins: Vec<crate::types::SDKPluginConfig>,
    pub max_thinking_tokens: Option<i32>,
    pub thinking: Option<crate::types::ThinkingConfig>,
    pub effort: Option<String>,
    pub output_format: Option<serde_json::Map<String, serde_json::Value>>,
    pub enable_file_checkpointing: bool,
}

impl From<&ClaudeAgentOptions> for TransportOptions {
    fn from(opts: &ClaudeAgentOptions) -> Self {
        Self {
            tools: opts.tools.clone(),
            tools_preset: opts.tools_preset.clone(),
            allowed_tools: opts.allowed_tools.clone(),
            system_prompt: opts.system_prompt.clone(),
            system_prompt_preset: opts.system_prompt_preset.clone(),
            system_prompt_file: opts.system_prompt_file.clone(),
            mcp_servers: opts.mcp_servers.clone(),
            permission_mode: opts.permission_mode,
            continue_conversation: opts.continue_conversation,
            resume: opts.resume.clone(),
            fork_session: opts.fork_session,
            max_turns: opts.max_turns,
            max_budget_usd: opts.max_budget_usd,
            disallowed_tools: opts.disallowed_tools.clone(),
            model: opts.model.clone(),
            fallback_model: opts.fallback_model.clone(),
            betas: opts.betas.clone(),
            permission_prompt_tool_name: opts.permission_prompt_tool_name.clone(),
            cwd: opts.cwd.clone(),
            cli_path: opts.cli_path.clone(),
            settings: opts.settings.clone(),
            add_dirs: opts.add_dirs.clone(),
            env: opts.env.clone(),
            extra_args: opts.extra_args.clone(),
            max_buffer_size: opts.max_buffer_size,
            user: opts.user.clone(),
            include_partial_messages: opts.include_partial_messages,
            setting_sources: opts.setting_sources.clone(),
            plugins: opts.plugins.clone(),
            max_thinking_tokens: opts.max_thinking_tokens,
            thinking: opts.thinking.clone(),
            effort: opts.effort.clone(),
            output_format: opts.output_format.clone(),
            enable_file_checkpointing: opts.enable_file_checkpointing,
        }
    }
}

impl SubprocessCLITransport {
    pub fn new(options: TransportOptions) -> Self {
        let max_buffer_size = options.max_buffer_size.unwrap_or(DEFAULT_MAX_BUFFER_SIZE);
        Self {
            options,
            max_buffer_size,
            child: None,
            stdin: None,
            stdout_reader: None,
            stderr: Arc::new(Mutex::new(String::new())),
        }
    }

    fn resolve_cli_path(&self) -> Result<String> {
        if let Some(ref path) = self.options.cli_path {
            return Ok(path.clone());
        }
        match which::which(DEFAULT_CLI_NAME) {
            Ok(path) => Ok(path.to_string_lossy().to_string()),
            Err(_) => Err(ClaudeSDKError::CLINotFound(CLINotFoundError::new(
                "Claude Code not found",
                DEFAULT_CLI_NAME
            ))),
        }
    }

    fn build_args(&self) -> Result<Vec<String>> {
        let mut args = vec![
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
        ];

        if let Some(ref file) = self.options.system_prompt_file {
            args.push("--system-prompt-file".to_string());
            args.push(file.path.clone());
        } else if let Some(ref preset) = self.options.system_prompt_preset {
            if let Some(ref append) = preset.append {
                args.push("--append-system-prompt".to_string());
                args.push(append.clone());
            }
        } else {
            let prompt = self.options.system_prompt.as_deref().unwrap_or("");
            args.push("--system-prompt".to_string());
            args.push(prompt.to_string());
        }

        if let Some(ref preset) = self.options.tools_preset {
            let preset_name = if preset.preset.is_empty() || preset.preset == "claude_code" {
                "default"
            } else {
                &preset.preset
            };
            args.push("--tools".to_string());
            args.push(preset_name.to_string());
        } else if !self.options.tools.is_empty() {
            args.push("--tools".to_string());
            args.push(self.options.tools.join(","));
        }

        if !self.options.allowed_tools.is_empty() {
            args.push("--allowedTools".to_string());
            args.push(self.options.allowed_tools.join(","));
        }

        if !self.options.disallowed_tools.is_empty() {
            args.push("--disallowedTools".to_string());
            args.push(self.options.disallowed_tools.join(","));
        }

        if let Some(turns) = self.options.max_turns {
            args.push("--max-turns".to_string());
            args.push(turns.to_string());
        }

        if let Some(budget) = self.options.max_budget_usd {
            args.push("--max-budget-usd".to_string());
            args.push(budget.to_string());
        }

        if let Some(ref model) = self.options.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        if let Some(ref fallback) = self.options.fallback_model {
            args.push("--fallback-model".to_string());
            args.push(fallback.clone());
        }

        if !self.options.betas.is_empty() {
            args.push("--betas".to_string());
            let betas: Vec<String> = self.options.betas.iter()
                .map(|b| format!("{:?}", b).to_lowercase())
                .collect();
            args.push(betas.join(","));
        }

        if let Some(ref name) = self.options.permission_prompt_tool_name {
            args.push("--permission-prompt-tool".to_string());
            args.push(name.clone());
        }

        if let Some(mode) = self.options.permission_mode {
            args.push("--permission-mode".to_string());
            args.push(format!("{:?}", mode).to_lowercase());
        }

        if self.options.continue_conversation {
            args.push("--continue".to_string());
        }

        if let Some(ref resume) = self.options.resume {
            args.push("--resume".to_string());
            args.push(resume.clone());
        }

        if let Some(ref settings) = self.options.settings {
            args.push("--settings".to_string());
            args.push(settings.clone());
        }

        for dir in &self.options.add_dirs {
            args.push("--add-dir".to_string());
            args.push(dir.clone());
        }

        if !self.options.mcp_servers.is_empty() {
            let mcp_config = serde_json::json!({
                "mcpServers": self.options.mcp_servers
            });
            args.push("--mcp-config".to_string());
            args.push(mcp_config.to_string());
        }

        if self.options.include_partial_messages {
            args.push("--include-partial-messages".to_string());
        }

        if self.options.fork_session {
            args.push("--fork-session".to_string());
        }

        for plugin in &self.options.plugins {
            if !plugin.path.is_empty() {
                args.push("--plugin-dir".to_string());
                args.push(plugin.path.clone());
            }
        }

        let max_thinking = self.resolve_max_thinking_tokens();
        if let Some(tokens) = max_thinking {
            args.push("--max-thinking-tokens".to_string());
            args.push(tokens.to_string());
        }

        if let Some(ref effort) = self.options.effort {
            args.push("--effort".to_string());
            args.push(effort.clone());
        }

        let mut extra_keys: Vec<&String> = self.options.extra_args.keys().collect();
        extra_keys.sort();
        for key in extra_keys {
            args.push(format!("--{}", key));
            if let Some(ref value) = self.options.extra_args[key] {
                args.push(value.clone());
            }
        }

        Ok(args)
    }

    fn build_env(&self) -> std::collections::HashMap<String, String> {
        let mut env = std::env::vars().collect::<std::collections::HashMap<_, _>>();

        env.insert("CLAUDE_CODE_ENTRYPOINT".to_string(), DEFAULT_ENTRY_POINT.to_string());

        if self.options.enable_file_checkpointing {
            env.insert("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".to_string(), "true".to_string());
        }

        if let Some(ref cwd) = self.options.cwd {
            env.insert("PWD".to_string(), cwd.clone());
        }

        for (key, value) in &self.options.env {
            env.insert(key.clone(), value.clone());
        }

        env
    }

    fn resolve_max_thinking_tokens(&self) -> Option<i32> {
        if let Some(ref thinking) = self.options.thinking {
            match thinking.r#type {
                crate::types::ThinkingConfigType::Adaptive => {
                    self.options.max_thinking_tokens.or(Some(32000))
                }
                crate::types::ThinkingConfigType::Enabled => thinking.budget_tokens,
                crate::types::ThinkingConfigType::Disabled => Some(0),
            }
        } else {
            self.options.max_thinking_tokens
        }
    }

    async fn finish_read(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(ref mut child) = self.child {
            match child.wait().await {
                Ok(status) => {
                    if !status.success() {
                        let stderr = self.stderr.lock().await.clone();
                        return Err(ProcessError::new(
                            "Claude Code process exited with error",
                            status.code(),
                            stderr
                        ).into());
                    }
                }
                Err(e) => {
                    return Err(CLIConnectionError::new(format!("failed to wait for process: {}", e)).into());
                }
            }
        }
        Ok(None)
    }
}

#[async_trait]
impl Transport for SubprocessCLITransport {
    async fn connect(&mut self) -> Result<()> {
        let cli_path = self.resolve_cli_path()?;

        if let Some(ref cwd) = self.options.cwd {
            if !tokio::fs::metadata(cwd).await.map(|m| m.is_dir()).unwrap_or(false) {
                return Err(CLIConnectionError::new(
                    format!("working directory does not exist: {}", cwd)
                ).into());
            }
        }

        let args = self.build_args()?;
        let env = self.build_env();

        let mut cmd = Command::new(&cli_path);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(ref cwd) = self.options.cwd {
            cmd.current_dir(cwd);
        }

        for (key, value) in &env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ClaudeSDKError::CLINotFound(CLINotFoundError::new("Claude Code not found", cli_path))
            } else {
                CLIConnectionError::new(format!("failed to start Claude Code: {}", e)).into()
            }
        })?;

        let stdin = child.stdin.take()
            .ok_or_else(|| CLIConnectionError::new("failed to open CLI stdin"))?;
        let stdout = child.stdout.take()
            .ok_or_else(|| CLIConnectionError::new("failed to open CLI stdout"))?;
        let stderr = child.stderr.take()
            .ok_or_else(|| CLIConnectionError::new("failed to open CLI stderr"))?;

        let stderr_arc = self.stderr.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 { break; }
                let mut stderr_guard = stderr_arc.lock().await;
                stderr_guard.push_str(&line);
                line.clear();
            }
        });

        self.child = Some(child);
        self.stdin = Some(stdin);
        self.stdout_reader = Some(BufReader::new(stdout));

        Ok(())
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        let stdin = self.stdin.as_mut()
            .ok_or_else(|| CLIConnectionError::new("transport is not connected"))?;

        stdin.write_all(data).await
            .map_err(|e| CLIConnectionError::new(format!("failed to write to stdin: {}", e)))?;
        stdin.flush().await
            .map_err(|e| CLIConnectionError::new(format!("failed to flush stdin: {}", e)))?;

        Ok(())
    }

    async fn close_input(&mut self) -> Result<()> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin.shutdown().await
                .map_err(|e| CLIConnectionError::new(format!("failed to close stdin: {}", e)))?;
        }
        Ok(())
    }

    async fn read(&mut self) -> Result<Option<Vec<u8>>> {
        let reader = self.stdout_reader.as_mut()
            .ok_or_else(|| CLIConnectionError::new("transport is not connected"))?;

        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => {
                self.finish_read().await
            }
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                if !trimmed.starts_with("{") {
                    return Ok(None);
                }
                if trimmed.len() > self.max_buffer_size {
                    return Err(CLIJSONDecodeError::new(
                        format!("JSON message exceeded maximum buffer size of {} bytes", self.max_buffer_size),
                        serde_json::from_str::<serde_json::Value>("null").unwrap_err()
                    ).into());
                }
                serde_json::from_str::<serde_json::Value>(trimmed)
                    .map_err(|e| CLIJSONDecodeError::new(trimmed, e))?;

                Ok(Some(trimmed.as_bytes().to_vec()))
            }
            Err(e) => Err(CLIConnectionError::new(format!("failed reading stdout: {}", e)).into()),
        }
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.close_input().await;

        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }

        Ok(())
    }
}
