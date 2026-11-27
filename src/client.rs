//! Main client for interactive sessions with Claude CLI (Python
//! `ClaudeSDKClient`).
//!
//! [`ClaudeAgentClient::connect`] starts a background runtime that reads the
//! CLI's stdout for the rest of the session: control requests (`can_use_tool`,
//! hooks, SDK MCP tools) are answered as they arrive, even while the caller is
//! not reading, and messages are queued for [`ClaudeAgentClient::receive_messages`]
//! / [`ClaudeAgentClient::receive_response`], which stream them as they come.
//! The receive streams do not borrow the client, so control methods such as
//! [`ClaudeAgentClient::interrupt`] can be called while a turn is streaming.

use futures::{Stream, StreamExt, TryStreamExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::client_stream::stream_events_from_message;
use crate::client_types::{MessageResponse, StreamEvent};
use crate::error::{CLIConnectionError, Result};
use crate::internal::control::initialize_timeout_duration;
use crate::internal::runtime::{MessageStream, Runtime, RuntimeConfig};
use crate::internal::session_resume::{
    apply_materialized_options, materialize_resume_session, MaterializedResume,
};
use crate::internal::session_store_validation::validate_session_store_options;
use crate::internal::transport::{SubprocessCLITransport, Transport, TransportOptions};
use crate::types::config::warn_if_can_use_tool_shadowed;
use crate::types::{
    ClaudeAgentOptions, ContentBlock, ContextUsageResponse, MCPStatusResponse, Message,
    PermissionMode, UserMessageInput,
};

type JsonMap = serde_json::Map<String, serde_json::Value>;

/// Timeout for control requests other than `initialize` (Python default).
const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// The `session_id` Python's client stamps on prompts unless told otherwise.
const DEFAULT_SESSION_ID: &str = "default";

pub struct ClaudeAgentClient {
    options: ClaudeAgentOptions,
    /// A caller-supplied transport: CLI-flag options and store-backed resume
    /// do not apply to it (as upstream).
    custom_transport: bool,
    /// The transport while disconnected (a custom one is kept for reconnects;
    /// the subprocess transport is rebuilt on every connect).
    transport: Option<Box<dyn Transport>>,
    runtime: Option<Runtime>,
    initialization_result: Option<JsonMap>,
    history: Arc<Mutex<Vec<Message>>>,
    /// Fallback session id for [`StreamEvent`]s whose message carries none.
    session_id: String,
    materialized_resume: Option<MaterializedResume>,
}

impl std::fmt::Debug for ClaudeAgentClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeAgentClient")
            .field("session_id", &self.session_id)
            .field("connected", &self.runtime.is_some())
            .finish_non_exhaustive()
    }
}

impl ClaudeAgentClient {
    /// Fire-and-forget streaming for a single prompt.
    ///
    /// Spawns a background task that creates a fresh client from `options`,
    /// connects, sends `content` and forwards each [`StreamEvent`] of the turn
    /// to the returned receiver, then disconnects. The spawned task owns the
    /// client, so the caller only needs to hold the receiver. Connection or
    /// streaming failures are surfaced as [`StreamEvent::Error`].
    ///
    /// Accepts any [`UserMessageInput`]: a plain `String`/`&str` for text-only
    /// prompts, or a `Vec<InputContentBlock>` to deliver text plus images.
    ///
    /// Must be called from within a Tokio runtime.
    pub fn spawn_stream_message(
        options: ClaudeAgentOptions,
        content: impl Into<UserMessageInput>,
    ) -> mpsc::UnboundedReceiver<StreamEvent> {
        let content = content.into();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut client = match Self::new(options) {
                Ok(client) => client,
                Err(err) => {
                    let _ = tx.send(StreamEvent::Error(err.to_string()));
                    return;
                }
            };
            let result = async {
                client.connect().await?;
                client.query(content).await?;
                forward_events(client.receive_response(), client.session_id.clone(), &tx).await;
                Ok::<(), crate::error::ClaudeSDKError>(())
            }
            .await;
            if let Err(err) = result {
                let _ = tx.send(StreamEvent::Error(err.to_string()));
            }
            // The turn is over: end the receiver now, then let the CLI exit.
            drop(tx);
            if let Err(err) = client.disconnect().await {
                tracing::debug!("error closing the streamed session: {err}");
            }
        });
        rx
    }

    /// A client for the default subprocess transport.
    pub fn new(options: ClaudeAgentOptions) -> Result<Self> {
        Self::build(options, None)
    }

    /// A client for a caller-supplied transport.
    ///
    /// The SDK calls its `connect()`, writes prompts and control messages to
    /// it and reads the CLI's output from it; options that become CLI flags
    /// are not applied to it, and store-backed resume is skipped.
    pub fn with_transport(
        options: ClaudeAgentOptions,
        transport: Box<dyn Transport>,
    ) -> Result<Self> {
        Self::build(options, Some(transport))
    }

    fn build(options: ClaudeAgentOptions, transport: Option<Box<dyn Transport>>) -> Result<Self> {
        validate_session_store_options(&options)?;
        let session_id = options
            .session_id
            .clone()
            .or_else(|| options.resume.clone())
            .unwrap_or_else(|| DEFAULT_SESSION_ID.to_string());
        Ok(Self {
            custom_transport: transport.is_some(),
            transport,
            options,
            runtime: None,
            initialization_result: None,
            history: Arc::new(Mutex::new(Vec::new())),
            session_id,
            materialized_resume: None,
        })
    }

    /// Connect to the CLI and run the initialize handshake.
    ///
    /// On failure after the CLI was started (for example an initialize
    /// timeout or an `on_failure="block"` hook the CLI cannot honor) the
    /// session is torn down before the error is returned. Calling `connect`
    /// on a connected client is a no-op.
    pub async fn connect(&mut self) -> Result<()> {
        if self.runtime.is_some() {
            return Ok(());
        }
        validate_session_store_options(&self.options)?;
        warn_if_can_use_tool_shadowed(&self.options);
        let result = self.connect_inner().await;
        if result.is_err() {
            let _ = self.disconnect().await;
        }
        result
    }

    async fn connect_inner(&mut self) -> Result<()> {
        let mut options = self.options.clone();
        if !self.custom_transport {
            self.materialized_resume = materialize_resume_session(&options).await?;
            if let Some(materialized) = &self.materialized_resume {
                options = apply_materialized_options(&options, materialized);
            }
            self.transport = Some(Box::new(SubprocessCLITransport::new(
                TransportOptions::from(&options),
            )));
        }
        let mut transport = self.transport.take().ok_or_else(|| {
            CLIConnectionError::new("The transport is gone; create a new client to reconnect.")
        })?;
        if let Err(error) = transport.connect().await {
            let _ = transport.close().await;
            self.transport = Some(transport);
            return Err(error);
        }

        let config = RuntimeConfig::from_options(&options, !self.custom_transport);
        let (runtime, init) = Runtime::start(transport, config);
        let runtime = self.runtime.insert(runtime);
        let response = runtime
            .finish_initialize(init, initialize_timeout_duration())
            .await?;
        self.initialization_result = Some(response);
        Ok(())
    }

    /// Connect and send `content` as the first prompt (Python
    /// `connect(prompt: str)`).
    pub async fn connect_with_prompt(
        &mut self,
        content: impl Into<UserMessageInput>,
    ) -> Result<()> {
        self.connect().await?;
        self.query(content).await
    }

    /// Connect and stream `stream` as the prompt (Python
    /// `connect(prompt: AsyncIterable)`).
    ///
    /// The messages are written by a background task, as given (apart from
    /// the `verbatim_prompts` stamp). Once the stream ends stdin is closed:
    /// when the run ends if SDK MCP servers, hooks or `can_use_tool` need the
    /// control channel, otherwise at once.
    pub async fn connect_with_stream<S>(&mut self, stream: S) -> Result<()>
    where
        S: Stream<Item = serde_json::Value> + Send + 'static,
    {
        self.connect().await?;
        self.runtime()?.spawn_stream_input(stream);
        Ok(())
    }

    fn runtime(&self) -> Result<&Runtime> {
        match (&self.runtime, &self.initialization_result) {
            (Some(runtime), Some(_)) => Ok(runtime),
            _ => Err(CLIConnectionError::new("Not connected. Call connect() first.").into()),
        }
    }

    /// Send `content` and collect the turn into a [`MessageResponse`].
    pub async fn send_message(
        &self,
        content: impl Into<UserMessageInput>,
    ) -> Result<MessageResponse> {
        self.query(content).await?;
        let messages = self.collect_response().await?;
        let mut content_parts: Vec<String> = Vec::new();
        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut usage: Option<HashMap<String, serde_json::Value>> = None;
        let mut stop_reason: Option<String> = None;
        let mut model = String::new();
        let mut session_id: Option<String> = None;

        for message in messages {
            match message {
                Message::AssistantMsg {
                    content: assistant_content,
                    session_id: message_session,
                    ..
                } => {
                    if model.is_empty() {
                        model.clone_from(&assistant_content.model);
                    }
                    if session_id.is_none() {
                        session_id = message_session;
                    }
                    for block in &assistant_content.content {
                        match block {
                            ContentBlock::Text { text } => content_parts.push(text.clone()),
                            ContentBlock::Thinking { thinking, .. } => {
                                content_parts.push(thinking.clone())
                            }
                            _ => {}
                        }
                        blocks.push(block.clone());
                    }
                }
                Message::ResultMsg {
                    stop_reason: reason,
                    usage: u,
                    session_id: result_session,
                    ..
                } => {
                    stop_reason = reason;
                    if let Some(u) = u {
                        usage = Some(u.into_iter().collect());
                    }
                    session_id = Some(result_session);
                }
                _ => {}
            }
        }

        Ok(MessageResponse {
            content: content_parts.join(""),
            blocks,
            model,
            stop_reason,
            session_id: session_id.unwrap_or_else(|| self.session_id.clone()),
            usage,
        })
    }

    /// Send a prompt with `session_id` `"default"` (Python
    /// `query(prompt: str)`).
    pub async fn query(&self, content: impl Into<UserMessageInput>) -> Result<()> {
        self.query_with_session_id(content, DEFAULT_SESSION_ID)
            .await
    }

    /// Send a prompt for `session_id`.
    pub async fn query_with_session_id(
        &self,
        content: impl Into<UserMessageInput>,
        session_id: impl Into<String>,
    ) -> Result<()> {
        let runtime = self.runtime()?;
        let message = user_message(&content.into(), &session_id.into());
        let shared = runtime.shared();
        shared.write_json(&shared.stamp(message)).await
    }

    /// Stream prompt messages, filling in `session_id` `"default"` where a
    /// message has none (Python `query(prompt: AsyncIterable)`).
    pub async fn query_stream<S>(&self, stream: S) -> Result<()>
    where
        S: Stream<Item = serde_json::Value>,
    {
        self.query_stream_with_session_id(stream, DEFAULT_SESSION_ID)
            .await
    }

    /// [`Self::query_stream`] with an explicit default `session_id`.
    pub async fn query_stream_with_session_id<S>(
        &self,
        stream: S,
        session_id: impl Into<String>,
    ) -> Result<()>
    where
        S: Stream<Item = serde_json::Value>,
    {
        let shared = Arc::clone(self.runtime()?.shared());
        let session_id = session_id.into();
        futures::pin_mut!(stream);
        while let Some(mut message) = stream.next().await {
            if let Some(object) = message.as_object_mut() {
                object
                    .entry("session_id")
                    .or_insert_with(|| serde_json::Value::String(session_id.clone()));
            }
            shared.write_json(&shared.stamp(message)).await?;
        }
        Ok(())
    }

    /// Stream every message as it arrives until the CLI's output ends
    /// (Python `receive_messages()`).
    ///
    /// Unknown message types are skipped; a malformed known message is
    /// yielded as an `Err` and the stream goes on. A fatal reader error (the
    /// CLI failing, or a [`crate::ResultError`] after an error result) is the
    /// last item. The stream does not borrow the client.
    pub fn receive_messages(&self) -> MessageStream {
        self.message_stream(false, false)
    }

    /// Stream messages up to and including the next `ResultMessage` (Python
    /// `receive_response()`).
    pub fn receive_response(&self) -> MessageStream {
        self.message_stream(true, false)
    }

    /// Collect [`Self::receive_response`] into a `Vec`, failing on the first
    /// error.
    pub async fn collect_response(&self) -> Result<Vec<Message>> {
        self.receive_response().try_collect().await
    }

    /// Collect [`Self::receive_messages`] into a `Vec` (until the CLI's
    /// output ends), failing on the first error.
    pub async fn collect_messages(&self) -> Result<Vec<Message>> {
        self.receive_messages().try_collect().await
    }

    fn message_stream(&self, stop_at_result: bool, exclusive: bool) -> MessageStream {
        let runtime = match self.runtime() {
            Ok(runtime) => runtime,
            Err(error) => return futures::stream::once(async move { Err(error) }).boxed(),
        };
        let history = Arc::clone(&self.history);
        let messages = if exclusive {
            runtime.exclusive_messages(stop_at_result)
        } else {
            runtime.messages(stop_at_result)
        };
        messages
            .inspect(move |item| {
                if let Ok(message) = item {
                    history
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(message.clone());
                }
            })
            .boxed()
    }

    /// Send `content` and stream the turn as [`StreamEvent`]s.
    ///
    /// Returns once the prompt is written; a background task forwards events
    /// as the messages arrive, until the turn's result. Errors (including a
    /// malformed message) arrive as [`StreamEvent::Error`].
    pub async fn stream_message(
        &self,
        content: impl Into<UserMessageInput>,
    ) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
        self.query(content).await?;
        let (tx, rx) = mpsc::unbounded_channel();
        // The forwarder owns the queue for the whole turn, so back-to-back
        // stream_message calls each get their own turn, in order.
        let messages = self.message_stream(true, true);
        let session_id = self.session_id.clone();
        tokio::spawn(async move { forward_events(messages, session_id, &tx).await });
        Ok(rx)
    }

    /// The messages received through the receive streams so far.
    pub async fn get_conversation_history(&self) -> Result<Vec<Message>> {
        Ok(self
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }

    /// Disconnect (alias of [`Self::disconnect`]).
    pub async fn abort(&mut self) -> Result<()> {
        self.disconnect().await
    }

    /// Close the session: flush the transcript mirror, cancel in-flight
    /// control handlers, close the transport (stdin EOF, then SIGTERM/SIGKILL
    /// if the CLI does not exit) and remove a materialized resume directory.
    pub async fn disconnect(&mut self) -> Result<()> {
        let mut result = Ok(());
        if let Some(runtime) = self.runtime.take() {
            let (closed, transport) = runtime.close().await;
            result = closed;
            if self.custom_transport {
                self.transport = transport;
            }
        }
        self.initialization_result = None;
        if let Some(materialized) = self.materialized_resume.take() {
            materialized.cleanup().await;
        }
        result
    }

    /// Disconnect and drop the client.
    pub async fn close(mut self) -> Result<()> {
        self.disconnect().await
    }

    async fn control(&self, request: serde_json::Value) -> Result<JsonMap> {
        self.runtime()?
            .shared()
            .send_control_request(request, CONTROL_REQUEST_TIMEOUT)
            .await
    }

    pub async fn interrupt(&self) -> Result<()> {
        self.control(serde_json::json!({"subtype": "interrupt"}))
            .await?;
        Ok(())
    }

    pub async fn set_permission_mode(&self, mode: PermissionMode) -> Result<()> {
        self.control(serde_json::json!({
            "subtype": "set_permission_mode",
            "mode": mode,
        }))
        .await?;
        Ok(())
    }

    pub async fn set_model(&self, model: Option<String>) -> Result<()> {
        self.control(serde_json::json!({
            "subtype": "set_model",
            "model": model,
        }))
        .await?;
        Ok(())
    }

    pub async fn rewind_files(&self, user_message_id: impl Into<String>) -> Result<()> {
        self.control(serde_json::json!({
            "subtype": "rewind_files",
            "user_message_id": user_message_id.into(),
        }))
        .await?;
        Ok(())
    }

    pub async fn reconnect_mcp_server(&self, server_name: impl Into<String>) -> Result<()> {
        self.control(serde_json::json!({
            "subtype": "mcp_reconnect",
            "serverName": server_name.into(),
        }))
        .await?;
        Ok(())
    }

    pub async fn toggle_mcp_server(
        &self,
        server_name: impl Into<String>,
        enabled: bool,
    ) -> Result<()> {
        self.control(serde_json::json!({
            "subtype": "mcp_toggle",
            "serverName": server_name.into(),
            "enabled": enabled,
        }))
        .await?;
        Ok(())
    }

    pub async fn stop_task(&self, task_id: impl Into<String>) -> Result<()> {
        self.control(serde_json::json!({
            "subtype": "stop_task",
            "task_id": task_id.into(),
        }))
        .await?;
        Ok(())
    }

    pub async fn get_mcp_status(&self) -> Result<MCPStatusResponse> {
        let response = self
            .control(serde_json::json!({"subtype": "mcp_status"}))
            .await?;
        Ok(serde_json::from_value(serde_json::Value::Object(response))?)
    }

    pub async fn get_context_usage(&self) -> Result<ContextUsageResponse> {
        let response = self
            .control(serde_json::json!({"subtype": "get_context_usage"}))
            .await?;
        Ok(serde_json::from_value(serde_json::Value::Object(response))?)
    }

    /// The initialize response (commands, output styles, capabilities).
    ///
    /// Fails with `CLIConnectionError` when not connected, as upstream.
    pub fn get_server_info(&self) -> Result<Option<&JsonMap>> {
        if self.runtime.is_none() {
            return Err(CLIConnectionError::new("Not connected. Call connect() first.").into());
        }
        Ok(self.initialization_result.as_ref())
    }
}

impl Drop for ClaudeAgentClient {
    /// A client dropped while connected is closed in the background (the CLI
    /// is killed outright if no Tokio runtime is available).
    fn drop(&mut self) {
        let runtime = self.runtime.take();
        let materialized = self.materialized_resume.take();
        if runtime.is_none() && materialized.is_none() {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Some(runtime) = runtime {
                        let _ = runtime.close().await;
                    }
                    if let Some(materialized) = materialized {
                        materialized.cleanup().await;
                    }
                });
            }
            Err(_) => drop(runtime),
        }
    }
}

/// The user message Python's client writes for a prompt.
fn user_message(content: &UserMessageInput, session_id: &str) -> serde_json::Value {
    // `content` is a JSON string for text-only prompts, or an array of
    // content blocks (text + images) for multimodal prompts.
    serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": content.to_content_value()},
        "parent_tool_use_id": null,
        "session_id": session_id,
    })
}

/// Map a message stream to [`StreamEvent`]s until it ends or the receiver
/// is dropped.
async fn forward_events(
    mut messages: MessageStream,
    session_id: String,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) {
    while let Some(item) = messages.next().await {
        let events = match item {
            Ok(message) => stream_events_from_message(&message, &session_id),
            Err(error) => vec![StreamEvent::Error(error.to_string())],
        };
        for event in events {
            if tx.send(event).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_send<T: Send>() {}

    #[test]
    fn client_and_streams_are_thread_safe() {
        assert_send_sync::<ClaudeAgentClient>();
        assert_send::<MessageStream>();
    }
}
