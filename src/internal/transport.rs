use async_trait::async_trait;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::error::{CLIConnectionError, CLINotFoundError, ClaudeSDKError, ProcessError, Result};
use crate::internal::client_app::{DERIVED_CLIENT_APP_ENV, EXPLICIT_CLIENT_APP_ENV};
use crate::internal::stdout_decoder::StdoutDecoder;
use crate::types::ClaudeAgentOptions;

const DEFAULT_ENTRY_POINT: &str = "sdk-rust";
const DEFAULT_MAX_BUFFER_SIZE: usize = 1024 * 1024;
/// Asks the CLI for `session_state_changed` frames marked `sdk_host_only`,
/// which the read loop uses to tell when the run is over and keeps out of the
/// caller's stream.
pub(crate) const SDK_READS_SESSION_STATE_ENV: &str = "CLAUDE_CODE_SDK_READS_SESSION_STATE";
/// How long each shutdown stage (EOF, SIGTERM, SIGKILL) waits for the CLI.
const CLOSE_STAGE_TIMEOUT: Duration = Duration::from_secs(5);
/// Most stderr retained for `ProcessError` when stderr is piped.
const STDERR_TAIL_LIMIT: usize = 64 * 1024;
const STDERR_PLACEHOLDER: &str = "Check stderr output for details";

/// The connection to a Claude Code process (Python `Transport`): the
/// extension point for custom transports.
///
/// Once connected, the SDK moves the transport into a background task that
/// reads and writes concurrently: a pending [`Transport::read`] is dropped
/// whenever a write, [`Transport::close_input`] or [`Transport::close`] must
/// run, and `read` is then called again. **`read` must therefore be
/// cancel-safe**: dropping its future before it completes must not lose a
/// message (buffer partial data in `self`, as the subprocess transport does).
#[async_trait]
pub trait Transport: Send + Sync {
    /// Start the process / open the connection.
    async fn connect(&mut self) -> Result<()>;
    /// Write raw bytes (one or more newline-terminated JSON messages).
    async fn write(&mut self, data: &[u8]) -> Result<()>;
    /// End the input stream (close stdin for process transports); Python
    /// `end_input()`.
    async fn close_input(&mut self) -> Result<()>;
    /// The next stdout message as raw JSON bytes, or `None` at end of output.
    /// Must be cancel-safe (see the trait docs).
    async fn read(&mut self) -> Result<Option<Vec<u8>>>;
    /// Close the connection and release its resources. Implementations must
    /// bound their own awaits: the SDK waits for this to return.
    async fn close(&mut self) -> Result<()>;
    /// Whether the transport is ready for communication (Python
    /// `is_ready()`).
    fn is_ready(&self) -> bool {
        true
    }
}

#[derive(Debug)]
pub struct SubprocessCLITransport {
    options: TransportOptions,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout_reader: Option<BufReader<ChildStdout>>,
    stdout_decoder: StdoutDecoder,
    stderr: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
    exit_error: Option<String>,
    close_stage_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct TransportOptions {
    pub tools: Vec<String>,
    pub tools_set: bool,
    pub tools_preset: Option<crate::types::ToolsPreset>,
    pub allowed_tools: Vec<String>,
    pub system_prompt: Option<String>,
    pub system_prompt_preset: Option<crate::types::SystemPromptPreset>,
    pub system_prompt_file: Option<crate::types::SystemPromptFile>,
    pub system_prompt_custom: Option<crate::types::SystemPromptCustom>,
    pub mcp_servers: std::collections::HashMap<String, crate::types::MCPServerConfig>,
    pub mcp_servers_config: Option<String>,
    pub permission_mode: Option<crate::types::PermissionMode>,
    pub continue_conversation: bool,
    pub resume: Option<String>,
    pub session_id: Option<String>,
    pub fork_session: bool,
    pub resume_session_at: Option<String>,
    pub resume_drops_turn: Option<String>,
    pub max_turns: Option<i32>,
    pub max_budget_usd: Option<f64>,
    pub task_budget: Option<crate::types::TaskBudget>,
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
    pub extra_args: Vec<(String, Option<String>)>,
    pub max_buffer_size: Option<usize>,
    pub user: Option<String>,
    pub include_partial_messages: bool,
    pub include_hook_events: bool,
    pub strict_mcp_config: bool,
    pub setting_sources: Option<Vec<crate::types::SettingSource>>,
    pub skills: Option<crate::types::SkillsConfig>,
    pub sandbox: Option<crate::types::SandboxSettings>,
    pub plugins: Vec<crate::types::SDKPluginConfig>,
    pub max_thinking_tokens: Option<i32>,
    pub thinking: Option<crate::types::ThinkingConfig>,
    pub effort: Option<crate::types::EffortLevel>,
    pub output_format: Option<serde_json::Map<String, serde_json::Value>>,
    pub enable_file_checkpointing: bool,
    pub stderr: Option<crate::types::StderrCallback>,
    pub can_use_tool: Option<crate::types::CanUseToolCallback>,
    pub sdk_mcp_servers: std::collections::HashMap<String, crate::mcp::SimpleMCPServer>,
    pub session_store_enabled: bool,
    pub client_app: Option<String>,
    pub verbatim_prompts: bool,
}

impl From<&ClaudeAgentOptions> for TransportOptions {
    fn from(opts: &ClaudeAgentOptions) -> Self {
        Self {
            tools: opts.tools.clone(),
            tools_set: opts.tools_set,
            tools_preset: opts.tools_preset.clone(),
            allowed_tools: opts.allowed_tools.clone(),
            system_prompt: opts.system_prompt.clone(),
            system_prompt_preset: opts.system_prompt_preset.clone(),
            system_prompt_file: opts.system_prompt_file.clone(),
            system_prompt_custom: opts.system_prompt_custom.clone(),
            mcp_servers: opts.mcp_servers.clone(),
            mcp_servers_config: opts.mcp_servers_config.clone(),
            permission_mode: opts.permission_mode,
            continue_conversation: opts.continue_conversation,
            resume: opts.resume.clone(),
            session_id: opts.session_id.clone(),
            fork_session: opts.fork_session,
            resume_session_at: opts.resume_session_at.clone(),
            resume_drops_turn: opts.resume_drops_turn.clone(),
            max_turns: opts.max_turns,
            max_budget_usd: opts.max_budget_usd,
            task_budget: opts.task_budget.clone(),
            disallowed_tools: opts.disallowed_tools.clone(),
            model: opts.model.clone(),
            fallback_model: opts.fallback_model.clone(),
            betas: opts.betas.clone(),
            permission_prompt_tool_name: opts
                .permission_prompt_tool_name
                .clone()
                .or_else(|| opts.can_use_tool.as_ref().map(|_| "stdio".to_string())),
            cwd: opts.cwd.clone(),
            cli_path: opts.cli_path.clone(),
            settings: opts.settings.clone(),
            add_dirs: opts.add_dirs.clone(),
            env: opts.env.clone(),
            extra_args: opts.extra_args.clone(),
            max_buffer_size: opts.max_buffer_size,
            user: opts.user.clone(),
            include_partial_messages: opts.include_partial_messages,
            include_hook_events: opts.include_hook_events,
            strict_mcp_config: opts.strict_mcp_config,
            setting_sources: opts.setting_sources.clone(),
            skills: opts.skills.clone(),
            sandbox: opts.sandbox.clone(),
            plugins: opts.plugins.clone(),
            max_thinking_tokens: opts.max_thinking_tokens,
            thinking: opts.thinking.clone(),
            effort: opts.effort,
            output_format: opts.output_format.clone(),
            enable_file_checkpointing: opts.enable_file_checkpointing,
            stderr: opts.stderr.clone(),
            can_use_tool: opts.can_use_tool.clone(),
            sdk_mcp_servers: opts.sdk_mcp_servers.clone(),
            session_store_enabled: opts.session_store.is_some(),
            client_app: opts.client_app.clone(),
            verbatim_prompts: opts.verbatim_prompts,
        }
    }
}

impl SubprocessCLITransport {
    pub fn new(options: TransportOptions) -> Self {
        let max_buffer_size = options.max_buffer_size.unwrap_or(DEFAULT_MAX_BUFFER_SIZE);
        Self {
            options,
            child: None,
            stdin: None,
            stdout_reader: None,
            stdout_decoder: StdoutDecoder::new(max_buffer_size),
            stderr: Arc::new(Mutex::new(String::new())),
            stderr_task: None,
            exit_error: None,
            close_stage_timeout: CLOSE_STAGE_TIMEOUT,
        }
    }

    /// Override how long each shutdown stage waits (tests only).
    #[cfg(test)]
    pub(crate) fn set_close_stage_timeout(&mut self, timeout: Duration) {
        self.close_stage_timeout = timeout;
    }

    fn resolve_cli_path(&self) -> Result<String> {
        crate::internal::cli_discovery::find_cli_path(self.options.cli_path.as_deref())
    }

    fn build_args(&self) -> Result<Vec<String>> {
        crate::internal::cli_args::build_cli_args(&self.options)
    }

    fn build_env(&self) -> std::collections::HashMap<String, String> {
        build_process_env(std::env::vars(), &self.options)
    }
    async fn finish_read(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(ref mut child) = self.child {
            let returncode = match child.wait().await {
                Ok(status) => exit_code(status),
                Err(_) => -1,
            };
            if returncode != 0 {
                // Let the stderr reader drain what the CLI wrote before exiting.
                // Keep the handle only if it is still running: a JoinHandle
                // must not be polled again after it has completed.
                if let Some(mut task) = self.stderr_task.take() {
                    if tokio::time::timeout(Duration::from_millis(500), &mut task)
                        .await
                        .is_err()
                    {
                        self.stderr_task = Some(task);
                    }
                }
                let stderr = self.stderr.lock().await.clone();
                let stderr = if stderr.is_empty() {
                    STDERR_PLACEHOLDER.to_string()
                } else {
                    stderr
                };
                let error = ProcessError::new(
                    format!("Command failed with exit code {returncode}"),
                    Some(returncode),
                    stderr,
                );
                self.exit_error = Some(error.to_string());
                return Err(error.into());
            }
        }
        Ok(None)
    }
}

/// Python `Popen.returncode`: the exit code, or `-signal` for a child killed
/// by a signal.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return -signal;
        }
    }
    -1
}

/// Ask the child to exit (SIGTERM on unix; TerminateProcess elsewhere).
fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // SAFETY: `kill` has no memory-safety preconditions; the pid is
            // our own unreaped child, so it cannot have been recycled.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            return;
        }
    }
    let _ = child.start_kill();
}

/// Resolve `options.user` (a user name, or a numeric uid) to a uid.
#[cfg(unix)]
pub(crate) fn resolve_user_uid(user: &str) -> std::result::Result<u32, String> {
    let not_found = || {
        format!(
            "getpwnam(): name not found: {}",
            crate::internal::cli_args::py_repr(user)
        )
    };
    let name = std::ffi::CString::new(user).map_err(|_| not_found())?;
    // SAFETY: `passwd` is a plain C struct; all-zero is a valid value.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    loop {
        // SAFETY: every pointer is valid for the call, and `buf.len()` is the
        // size of the buffer `getpwnam_r` may write into.
        let rc = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < 1024 * 1024 {
            let len = buf.len() * 2;
            buf.resize(len, 0);
            continue;
        }
        break;
    }
    if !result.is_null() {
        return Ok(pwd.pw_uid);
    }
    // A Rust option is always a string, so accept a numeric uid too (Python
    // takes an int for that).
    user.parse::<u32>().map_err(|_| not_found())
}

fn apply_user(cmd: &mut Command, user: Option<&str>) -> Result<()> {
    let Some(user) = user else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        let uid = resolve_user_uid(user)
            .map_err(|e| CLIConnectionError::new(format!("Failed to start Claude Code: {e}")))?;
        cmd.uid(uid);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (cmd, user);
        Err(CLIConnectionError::new(
            "Failed to start Claude Code: the user option is only supported on POSIX",
        )
        .into())
    }
}

/// Keep the last `STDERR_TAIL_LIMIT` bytes of stderr, on a char boundary.
fn push_stderr_tail(tail: &mut String, line: &str) {
    tail.push_str(line);
    tail.push('\n');
    if tail.len() > STDERR_TAIL_LIMIT {
        let mut cut = tail.len() - STDERR_TAIL_LIMIT;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail.drain(..cut);
    }
}

/// Read the CLI's stderr until EOF, framing lines out of arbitrary chunks.
/// Each non-empty right-trimmed line goes to the callback (a panic in the
/// callback is contained to that line) and into the bounded tail. Invalid
/// UTF-8 is decoded lossily so the pipe is always drained.
async fn drain_stderr<R>(
    stderr: R,
    tail: Arc<Mutex<String>>,
    callback: Option<crate::types::StderrCallback>,
    max_buffer_size: usize,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let emit = |line: &[u8], tail: &mut String| {
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end();
        if line.is_empty() {
            return;
        }
        push_stderr_tail(tail, line);
        if let Some(callback) = &callback {
            let line = line.to_string();
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback.call(line)))
                .is_err()
            {
                tracing::debug!("stderr callback panicked; continuing");
            }
        }
    };

    let mut reader = BufReader::new(stderr);
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::debug!("stderr stream read failed: {}", e);
                break;
            }
        };
        pending.extend_from_slice(&chunk[..n]);
        let mut guard = tail.lock().await;
        let mut start = 0;
        while let Some(pos) = pending[start..].iter().position(|b| *b == b'\n') {
            emit(&pending[start..start + pos], &mut guard);
            start += pos + 1;
        }
        pending.drain(..start);
        // A producer that never emits a newline can't grow the buffer
        // without bound; flush it as a partial line instead.
        if pending.len() > max_buffer_size {
            emit(&pending, &mut guard);
            pending.clear();
        }
    }
    let mut guard = tail.lock().await;
    emit(&pending, &mut guard);
}

fn build_process_env<I>(inherited: I, options: &TransportOptions) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut env = inherited
        .into_iter()
        .filter(|(key, _)| key != "CLAUDECODE")
        .collect::<HashMap<_, _>>();

    env.insert(
        "CLAUDE_CODE_ENTRYPOINT".to_string(),
        DEFAULT_ENTRY_POINT.to_string(),
    );

    for (key, value) in &options.env {
        env.insert(key.clone(), value.clone());
    }

    env.insert(
        "CLAUDE_AGENT_SDK_VERSION".to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    );

    // A derived label from an outer SDK session names that session's app.
    // An explicit `client_app` counts even when empty: that is the opt-out.
    // (Upstream gates label derivation on a CLI version no release meets yet,
    // so none is derived here.)
    env.remove(DERIVED_CLIENT_APP_ENV);
    if let Some(client_app) = &options.client_app {
        env.insert(EXPLICIT_CLIENT_APP_ENV.to_string(), client_app.clone());
    }

    apply_otel_trace_context(&mut env, &options.env, active_otel_trace_context());

    // The read loop waits for the CLI's `session_state_changed` "idle" and
    // drops the `sdk_host_only` frames; ask for them unless the caller chose a
    // value, in any case.
    if !env
        .keys()
        .any(|key| key.to_uppercase() == SDK_READS_SESSION_STATE_ENV)
    {
        env.insert(SDK_READS_SESSION_STATE_ENV.to_string(), "1".to_string());
    }

    if options.enable_file_checkpointing {
        env.insert(
            "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".to_string(),
            "true".to_string(),
        );
    }

    if let Some(ref cwd) = options.cwd {
        env.insert("PWD".to_string(), cwd.clone());
    }

    env
}

fn apply_otel_trace_context(
    env: &mut HashMap<String, String>,
    explicit_env: &HashMap<String, String>,
    carrier: HashMap<String, String>,
) {
    if !carrier.contains_key("traceparent") {
        return;
    }

    for key in ["TRACEPARENT", "TRACESTATE"] {
        if !explicit_env.contains_key(key) {
            env.remove(key);
        }
    }

    for (key, value) in carrier {
        let env_key = key.to_ascii_uppercase();
        if !explicit_env.contains_key(&env_key) {
            env.insert(env_key, value);
        }
    }
}

#[cfg(feature = "otel")]
fn active_otel_trace_context() -> HashMap<String, String> {
    let mut carrier = HashMap::new();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject(&mut carrier);
    });
    carrier
}

#[cfg(not(feature = "otel"))]
fn active_otel_trace_context() -> HashMap<String, String> {
    HashMap::new()
}

#[async_trait]
impl Transport for SubprocessCLITransport {
    async fn connect(&mut self) -> Result<()> {
        if self.child.is_some() {
            return Ok(());
        }

        let cli_path = self.resolve_cli_path()?;
        // Validate the resolved CLI before spawning it.
        crate::internal::cli_discovery::reject_windows_batch_cli(&cli_path, cfg!(windows))?;

        let args = self.build_args()?;
        let env = self.build_env();

        if let Some(ref cwd) = self.options.cwd {
            if !tokio::fs::metadata(cwd)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                let error = format!("Working directory does not exist: {cwd}");
                self.exit_error = Some(error.clone());
                return Err(CLIConnectionError::new(error).into());
            }
        }

        // Pipe stderr only when the caller registered a callback; otherwise
        // the child inherits this process's stderr.
        let pipe_stderr = self.options.stderr.is_some();

        let mut cmd = Command::new(&cli_path);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if pipe_stderr {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            // Replace the environment wholesale: removals in build_env (such
            // as CLAUDECODE) must not be undone by inheritance.
            .env_clear()
            .envs(&env)
            // A transport dropped without close() must not orphan the CLI.
            .kill_on_drop(true);

        if let Some(ref cwd) = self.options.cwd {
            cmd.current_dir(cwd);
        }

        apply_user(&mut cmd, self.options.user.as_deref())?;

        // Retry briefly on ETXTBSY ("Text file busy"): on Linux a concurrent
        // fork elsewhere in the process can transiently hold a write handle to
        // a freshly written CLI script, making exec fail until it execs/exits.
        let mut spawn_result = cmd.spawn();
        for attempt in 1..=5u64 {
            match &spawn_result {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    tokio::time::sleep(std::time::Duration::from_millis(10 * attempt)).await;
                    spawn_result = cmd.spawn();
                }
                _ => break,
            }
        }
        let mut child = match spawn_result {
            Ok(child) => child,
            Err(e) => {
                let error: ClaudeSDKError = if e.kind() == std::io::ErrorKind::NotFound {
                    CLINotFoundError::new(
                        format!("Claude Code not found at: {cli_path}"),
                        cli_path.clone(),
                    )
                    .into()
                } else {
                    CLIConnectionError::new(format!("Failed to start Claude Code: {e}")).into()
                };
                self.exit_error = Some(error.to_string());
                return Err(error);
            }
        };

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| CLIConnectionError::new("Failed to start Claude Code: no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CLIConnectionError::new("Failed to start Claude Code: no stdout"))?;

        if let Some(stderr) = child.stderr.take() {
            let max_buffer_size = self
                .options
                .max_buffer_size
                .unwrap_or(DEFAULT_MAX_BUFFER_SIZE);
            self.stderr_task = Some(tokio::spawn(drain_stderr(
                stderr,
                self.stderr.clone(),
                self.options.stderr.clone(),
                max_buffer_size,
            )));
        }

        self.child = Some(child);
        self.stdin = Some(stdin);
        self.stdout_reader = Some(BufReader::new(stdout));
        self.exit_error = None;

        Ok(())
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        if self.stdin.is_none() {
            return Err(
                CLIConnectionError::new("ProcessTransport is not ready for writing").into(),
            );
        }

        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(CLIConnectionError::new(format!(
                    "Cannot write to terminated process (exit code: {})",
                    exit_code(status)
                ))
                .into());
            }
        }

        if let Some(error) = &self.exit_error {
            return Err(CLIConnectionError::new(format!(
                "Cannot write to process that exited with error: {error}"
            ))
            .into());
        }

        let Some(stdin) = self.stdin.as_mut() else {
            return Err(
                CLIConnectionError::new("ProcessTransport is not ready for writing").into(),
            );
        };
        let result = match stdin.write_all(data).await {
            Ok(()) => stdin.flush().await,
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            self.stdin = None;
            let message = format!("Failed to write to process stdin: {e}");
            self.exit_error = Some(message.clone());
            return Err(CLIConnectionError::new(message).into());
        }

        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.child.is_some() && self.exit_error.is_none()
    }

    async fn close_input(&mut self) -> Result<()> {
        if let Some(mut stdin) = self.stdin.take() {
            // Closing is best effort, as in the Python SDK's end_input().
            let _ = stdin.shutdown().await;
        }
        Ok(())
    }

    async fn read(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if let Some(data) = self.stdout_decoder.next() {
                return Ok(Some(data));
            }

            let mut chunk = [0u8; 8192];
            let read_result = {
                let reader = self
                    .stdout_reader
                    .as_mut()
                    .ok_or_else(|| CLIConnectionError::new("transport is not connected"))?;
                reader.read(&mut chunk).await
            };

            match read_result {
                Ok(0) => {
                    self.stdout_decoder.finish()?;
                    if let Some(data) = self.stdout_decoder.next() {
                        return Ok(Some(data));
                    }
                    return self.finish_read().await;
                }
                Ok(n) => self.stdout_decoder.push_bytes(&chunk[..n])?,
                Err(e) => {
                    return Err(
                        CLIConnectionError::new(format!("failed reading stdout: {}", e)).into(),
                    )
                }
            }
        }
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.close_input().await;

        if let Some(mut child) = self.child.take() {
            // After stdin EOF, give the CLI time to flush its session
            // transcript before escalating to SIGTERM, then SIGKILL.
            let stage = self.close_stage_timeout;
            if matches!(child.try_wait(), Ok(None))
                && tokio::time::timeout(stage, child.wait()).await.is_err()
            {
                terminate(&mut child);
                if tokio::time::timeout(stage, child.wait()).await.is_err() {
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(stage, child.wait()).await;
                }
            }
        }

        if let Some(mut task) = self.stderr_task.take() {
            // The pipe closes when the CLI exits; let the reader flush its
            // last partial line, but don't wait on a grandchild holding it.
            if tokio::time::timeout(Duration::from_millis(500), &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }

        self.stdout_reader = None;
        self.exit_error = None;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_env_matches_python_sdk_subprocess_defaults() {
        let options = crate::types::ClaudeAgentOptions::builder()
            .env_var("CLAUDE_CODE_ENTRYPOINT", "custom-entrypoint")
            .env_var("TRACEPARENT", "explicit-trace")
            .cwd("/tmp/project")
            .enable_file_checkpointing(true)
            .build();
        let transport_options = TransportOptions::from(&options);

        let env = build_process_env(
            [
                ("CLAUDECODE".to_string(), "1".to_string()),
                ("PATH".to_string(), "/bin".to_string()),
                ("TRACEPARENT".to_string(), "ambient-trace".to_string()),
            ],
            &transport_options,
        );

        assert_eq!(env.get("CLAUDECODE"), None);
        assert_eq!(
            env.get("CLAUDE_CODE_ENTRYPOINT").map(String::as_str),
            Some("custom-entrypoint")
        );
        assert_eq!(
            env.get("CLAUDE_AGENT_SDK_VERSION").map(String::as_str),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            env.get("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING")
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(env.get("PWD").map(String::as_str), Some("/tmp/project"));
        assert_eq!(
            env.get("TRACEPARENT").map(String::as_str),
            Some("explicit-trace")
        );
    }

    #[test]
    fn process_env_injects_active_otel_context_like_python_sdk() {
        let options = crate::types::ClaudeAgentOptions::builder().build();
        let transport_options = TransportOptions::from(&options);
        let mut env = build_process_env(
            [
                (
                    "TRACEPARENT".to_string(),
                    "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01".to_string(),
                ),
                ("TRACESTATE".to_string(), "vendor=stale".to_string()),
            ],
            &transport_options,
        );

        apply_otel_trace_context(
            &mut env,
            &transport_options.env,
            HashMap::from([
                (
                    "traceparent".to_string(),
                    "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
                ),
                ("tracestate".to_string(), "vendor=value".to_string()),
            ]),
        );

        assert_eq!(
            env.get("TRACEPARENT").map(String::as_str),
            Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")
        );
        assert_eq!(
            env.get("TRACESTATE").map(String::as_str),
            Some("vendor=value")
        );
    }

    #[test]
    fn process_env_preserves_explicit_traceparent_over_otel_context() {
        let options = crate::types::ClaudeAgentOptions::builder()
            .env_var("TRACEPARENT", "custom")
            .build();
        let transport_options = TransportOptions::from(&options);
        let mut env = build_process_env(
            [("TRACEPARENT".to_string(), "ambient".to_string())],
            &transport_options,
        );

        apply_otel_trace_context(
            &mut env,
            &transport_options.env,
            HashMap::from([("traceparent".to_string(), "active".to_string())]),
        );

        assert_eq!(env.get("TRACEPARENT").map(String::as_str), Some("custom"));
    }

    #[test]
    fn process_env_preserves_inherited_w3c_env_without_active_otel_span() {
        let options = crate::types::ClaudeAgentOptions::builder().build();
        let transport_options = TransportOptions::from(&options);
        let mut env = build_process_env(
            [
                ("TRACEPARENT".to_string(), "ambient".to_string()),
                ("TRACESTATE".to_string(), "vendor=abc".to_string()),
            ],
            &transport_options,
        );

        apply_otel_trace_context(
            &mut env,
            &transport_options.env,
            HashMap::from([("baggage".to_string(), "user.id=123".to_string())]),
        );

        assert_eq!(env.get("TRACEPARENT").map(String::as_str), Some("ambient"));
        assert_eq!(
            env.get("TRACESTATE").map(String::as_str),
            Some("vendor=abc")
        );
    }

    #[test]
    fn process_env_sets_client_app_and_strips_derived_label() {
        let inherited = || {
            [
                (
                    DERIVED_CLIENT_APP_ENV.to_string(),
                    "outer-app/1.0".to_string(),
                ),
                (EXPLICIT_CLIENT_APP_ENV.to_string(), "ambient".to_string()),
            ]
        };

        let options = crate::types::ClaudeAgentOptions::builder()
            .env_var(EXPLICIT_CLIENT_APP_ENV, "from-env")
            .client_app("my-app/1.0.0")
            .build();
        let env = build_process_env(inherited(), &TransportOptions::from(&options));
        assert_eq!(env.get(DERIVED_CLIENT_APP_ENV), None);
        assert_eq!(
            env.get(EXPLICIT_CLIENT_APP_ENV).map(String::as_str),
            Some("my-app/1.0.0")
        );

        // An empty value is the opt-out and still counts as set.
        let options = crate::types::ClaudeAgentOptions::builder()
            .client_app("")
            .build();
        let env = build_process_env(inherited(), &TransportOptions::from(&options));
        assert_eq!(
            env.get(EXPLICIT_CLIENT_APP_ENV).map(String::as_str),
            Some("")
        );

        // Without the option, an inherited explicit value passes through.
        let options = crate::types::ClaudeAgentOptions::builder().build();
        let env = build_process_env(inherited(), &TransportOptions::from(&options));
        assert_eq!(
            env.get(EXPLICIT_CLIENT_APP_ENV).map(String::as_str),
            Some("ambient")
        );
        assert_eq!(env.get(DERIVED_CLIENT_APP_ENV), None);
    }

    #[test]
    fn process_env_requests_session_state_frames_unless_caller_set_it() {
        let options = crate::types::ClaudeAgentOptions::builder().build();
        let env = build_process_env(Vec::new(), &TransportOptions::from(&options));
        assert_eq!(
            env.get(SDK_READS_SESSION_STATE_ENV).map(String::as_str),
            Some("1")
        );

        let options = crate::types::ClaudeAgentOptions::builder()
            .env_var("claude_code_sdk_reads_session_state", "0")
            .build();
        let env = build_process_env(Vec::new(), &TransportOptions::from(&options));
        assert_eq!(env.get(SDK_READS_SESSION_STATE_ENV), None);
        assert_eq!(
            env.get("claude_code_sdk_reads_session_state")
                .map(String::as_str),
            Some("0")
        );

        let env = build_process_env(
            [(SDK_READS_SESSION_STATE_ENV.to_string(), "0".to_string())],
            &TransportOptions::from(&crate::types::ClaudeAgentOptions::default()),
        );
        assert_eq!(
            env.get(SDK_READS_SESSION_STATE_ENV).map(String::as_str),
            Some("0")
        );
    }

    fn fake_cli(body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "claude-rust-transport-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude");
        let mut file = std::fs::File::create(&script).unwrap();
        write!(file, "#!/bin/sh\n{body}").unwrap();
        file.sync_all().unwrap();
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).unwrap();
        }
        (dir, script)
    }

    const RESULT_LINE: &str = r#"{"type":"result","subtype":"success","duration_ms":1,"duration_api_ms":1,"is_error":false,"num_turns":1,"session_id":"s"}"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_cli_does_not_inherit_removed_env_vars() {
        // These names are only ever read by the CLI, so setting them here
        // cannot affect other tests.
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var(DERIVED_CLIENT_APP_ENV, "outer-app/1.0");

        let (dir, script) = fake_cli(&format!(
            "env > \"$(dirname \"$0\")/env.txt\"\nprintf '%s\\n' '{RESULT_LINE}'\n"
        ));
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path(script.to_string_lossy().to_string())
            .client_app("my-app/2.0")
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        transport.connect().await.unwrap();
        while transport.read().await.unwrap().is_some() {}
        transport.close().await.unwrap();

        let env = std::fs::read_to_string(dir.join("env.txt")).unwrap();
        let lines: Vec<&str> = env.lines().collect();
        assert!(!lines.iter().any(|l| l.starts_with("CLAUDECODE=")), "{env}");
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("CLAUDE_AGENT_SDK_DERIVED_CLIENT_APP=")),
            "{env}"
        );
        assert!(lines.contains(&"CLAUDE_AGENT_SDK_CLIENT_APP=my-app/2.0"));
        assert!(lines.contains(&"CLAUDE_CODE_SDK_READS_SESSION_STATE=1"));
        assert!(lines.contains(&"CLAUDE_CODE_ENTRYPOINT=sdk-rust"));
        assert!(lines.iter().any(|l| l.starts_with("PATH=")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_stderr_callback_receives_lines() {
        use std::sync::{Arc, Mutex};

        let (dir, script) = fake_cli(&format!(
            "printf 'diagnostic line  \\n\\n' >&2\nprintf '%s\\n' '{RESULT_LINE}'\n"
        ));

        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = lines.clone();
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path(script.to_string_lossy().to_string())
            .stderr(move |line| captured.lock().unwrap().push(line))
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));

        transport.connect().await.unwrap();
        let message = transport.read().await.unwrap().expect("result");
        let value: serde_json::Value = serde_json::from_slice(&message).unwrap();
        assert_eq!(value["type"], "result");
        assert_eq!(transport.read().await.unwrap(), None);
        transport.close().await.unwrap();

        assert_eq!(*lines.lock().unwrap(), vec!["diagnostic line".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stderr_reader_frames_lines_and_survives_bad_utf8_and_panics() {
        use std::sync::{Arc as StdArc, Mutex as StdMutex};

        let input: &[u8] =
            b"first\r\n\n  \nbad \xff bytes\npanic\nafter panic\n0123456789abcdef-tail";
        let lines = StdArc::new(StdMutex::new(Vec::<String>::new()));
        let captured = lines.clone();
        let callback = crate::types::StderrCallback::new(move |line| {
            if line == "panic" {
                panic!("callback failure");
            }
            captured.lock().unwrap().push(line);
        });
        let tail = Arc::new(Mutex::new(String::new()));
        drain_stderr(input, tail.clone(), Some(callback), 8).await;

        assert_eq!(
            *lines.lock().unwrap(),
            vec![
                "first".to_string(),
                "bad \u{fffd} bytes".to_string(),
                "after panic".to_string(),
                "0123456789abcdef-tail".to_string(),
            ]
        );
        assert!(tail.lock().await.contains("after panic\n"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nonzero_exit_and_write_after_exit_match_python_messages() {
        let (dir, script) = fake_cli("exit 3\n");
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path(script.to_string_lossy().to_string())
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        transport.connect().await.unwrap();

        let err = transport.read().await.unwrap_err();
        match &err {
            ClaudeSDKError::Process(process) => {
                assert_eq!(process.message, "Command failed with exit code 3");
                assert_eq!(process.exit_code, Some(3));
                assert_eq!(process.stderr, "Check stderr output for details");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let err = transport.write(b"{}\n").await.unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot write to terminated process (exit code: 3)"),
            "{err}"
        );

        transport.close().await.unwrap();
        let err = transport.write(b"{}\n").await.unwrap_err();
        assert!(err
            .to_string()
            .contains("ProcessTransport is not ready for writing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nonzero_exit_with_stderr_callback_keeps_tail_and_closes_cleanly() {
        let (dir, script) = fake_cli("printf 'boom\\n' >&2\nexit 4\n");
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path(script.to_string_lossy().to_string())
            .stderr(|_line| {})
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        transport.connect().await.unwrap();
        match transport.read().await.unwrap_err() {
            ClaudeSDKError::Process(process) => {
                assert_eq!(process.exit_code, Some(4));
                assert_eq!(process.stderr, "boom\n");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        // A second read after the error and close() must not re-poll the
        // finished stderr task.
        let _ = transport.read().await;
        transport.close().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn signal_killed_child_reports_negative_exit_code() {
        let (dir, script) = fake_cli("kill -9 $$\n");
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path(script.to_string_lossy().to_string())
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        transport.connect().await.unwrap();
        match transport.read().await.unwrap_err() {
            ClaudeSDKError::Process(process) => {
                assert_eq!(process.exit_code, Some(-9));
                assert_eq!(process.message, "Command failed with exit code -9");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn close_lets_cli_exit_on_stdin_eof_before_signalling() {
        let (dir, script) = fake_cli(
            "d=\"$(dirname \"$0\")\"\ntrap 'echo term > \"$d/term\"; exit 0' TERM\ncat > /dev/null\nsleep 0.2\necho eof > \"$d/eof\"\n",
        );
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path(script.to_string_lossy().to_string())
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        transport.connect().await.unwrap();
        transport.close().await.unwrap();

        assert!(dir.join("eof").exists());
        assert!(!dir.join("term").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn close_escalates_to_sigterm_then_sigkill() {
        // Ignores stdin EOF: SIGTERM ends it.
        let (dir, script) = fake_cli(
            "d=\"$(dirname \"$0\")\"\ntrap 'echo term > \"$d/term\"; exit 0' TERM\nwhile true; do sleep 0.05; done\n",
        );
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path(script.to_string_lossy().to_string())
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        transport.set_close_stage_timeout(Duration::from_millis(300));
        transport.connect().await.unwrap();
        transport.close().await.unwrap();
        assert!(dir.join("term").exists());
        let _ = std::fs::remove_dir_all(&dir);

        // Ignores SIGTERM too: SIGKILL ends it, within the three stages.
        let (dir, script) = fake_cli("trap '' TERM\nwhile true; do sleep 0.05; done\n");
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path(script.to_string_lossy().to_string())
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        transport.set_close_stage_timeout(Duration::from_millis(300));
        transport.connect().await.unwrap();
        let started = std::time::Instant::now();
        transport.close().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn resolves_user_names_and_numeric_uids() {
        assert_eq!(resolve_user_uid("root"), Ok(0));
        assert_eq!(resolve_user_uid("4242"), Ok(4242));
        let err = resolve_user_uid("no-such-user-claude-sdk").unwrap_err();
        assert_eq!(err, "getpwnam(): name not found: 'no-such-user-claude-sdk'");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unknown_user_fails_before_spawn() {
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path("/bin/true")
            .user("no-such-user-claude-sdk")
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        let err = transport.connect().await.unwrap_err();
        assert!(err
            .to_string()
            .contains("Failed to start Claude Code: getpwnam(): name not found"));
    }

    #[tokio::test]
    async fn connect_error_messages_match_python_sdk() {
        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path("/definitely/not/a/claude")
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        match transport.connect().await.unwrap_err() {
            ClaudeSDKError::CLINotFound(err) => {
                assert_eq!(
                    err.message,
                    "Claude Code not found at: /definitely/not/a/claude"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let options = crate::types::ClaudeAgentOptions::builder()
            .cli_path("/bin/true")
            .cwd("/definitely/not/a/dir")
            .build();
        let mut transport = SubprocessCLITransport::new(TransportOptions::from(&options));
        let err = transport.connect().await.unwrap_err();
        assert!(err
            .to_string()
            .contains("Working directory does not exist: /definitely/not/a/dir"));
    }
}
