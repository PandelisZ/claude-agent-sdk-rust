//! Main client for interactive sessions with Claude CLI.

use futures::{Stream, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::client_stream::stream_events_from_message;
use crate::client_types::{MessageResponse, StreamEvent};
use crate::error::{CLIConnectionError, Result};
use crate::internal::control::{
    initialize_request, initialize_timeout_duration, respond_to_control_request,
    send_control_request_with_callbacks, send_control_request_with_callbacks_and_timeout,
    ControlCallbacks,
};
use crate::internal::parser::parse_message_line;
use crate::internal::session_resume::{
    apply_materialized_options, materialize_resume_session, MaterializedResume,
};
use crate::internal::session_store_validation::validate_session_store_options;
use crate::internal::transcript_mirror::TranscriptMirrorBatcher;
use crate::internal::transport::{SubprocessCLITransport, Transport, TransportOptions};
use crate::types::{
    ClaudeAgentOptions, ContentBlock, ContextUsageResponse, MCPStatusResponse, Message,
    PermissionMode,
};

#[derive(Debug)]
#[allow(dead_code)]
struct ClientState {
    messages: Vec<Message>,
    current_stream_buffer: String,
    is_streaming: bool,
    server_info: Option<HashMap<String, serde_json::Value>>,
}

pub struct ClaudeAgentClient {
    transport: Box<dyn Transport>,
    state: Arc<RwLock<ClientState>>,
    session_id: String,
    connected: bool,
    initialized: bool,
    initialization_result: Option<serde_json::Map<String, serde_json::Value>>,
    control_callbacks: ControlCallbacks,
    transcript_mirror: Option<TranscriptMirrorBatcher>,
    source_options: Option<ClaudeAgentOptions>,
    materialized_resume: Option<MaterializedResume>,
}

impl std::fmt::Debug for ClaudeAgentClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeAgentClient")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl ClaudeAgentClient {
    pub fn new(options: ClaudeAgentOptions) -> Result<Self> {
        validate_session_store_options(&options)?;
        let transport_options = TransportOptions::from(&options);
        let transport = SubprocessCLITransport::new(transport_options);
        let mut client = Self::with_transport(options.clone(), Box::new(transport))?;
        client.source_options = Some(options);
        Ok(client)
    }

    pub fn with_transport(
        options: ClaudeAgentOptions,
        transport: Box<dyn Transport>,
    ) -> Result<Self> {
        let session_id = options
            .session_id
            .clone()
            .or_else(|| options.resume.clone())
            .unwrap_or_else(|| "default".to_string());
        let state = Arc::new(RwLock::new(ClientState {
            messages: Vec::new(),
            current_stream_buffer: String::new(),
            is_streaming: false,
            server_info: None,
        }));
        Ok(Self {
            transport,
            state,
            session_id,
            connected: false,
            initialized: false,
            initialization_result: None,
            control_callbacks: ControlCallbacks::from_options(&options),
            transcript_mirror: TranscriptMirrorBatcher::from_options(&options),
            source_options: None,
            materialized_resume: None,
        })
    }

    pub async fn connect(&mut self) -> Result<()> {
        if !self.connected {
            self.materialize_resume_before_connect().await?;
            self.transport.connect().await?;
            self.connected = true;
        }
        self.ensure_initialized().await?;
        Ok(())
    }

    pub async fn connect_with_prompt(&mut self, content: impl Into<String>) -> Result<()> {
        self.connect().await?;
        let content = content.into();
        let payload = self.build_user_payload(&content, None)?;
        let mut json_payload = serde_json::to_vec(&payload)?;
        json_payload.push(b'\n');
        self.transport.write(&json_payload).await
    }

    pub async fn connect_with_stream<S>(&mut self, stream: S) -> Result<()>
    where
        S: Stream<Item = serde_json::Value> + Unpin,
    {
        self.connect().await?;
        self.write_message_stream(stream, "default").await
    }

    async fn materialize_resume_before_connect(&mut self) -> Result<()> {
        let Some(options) = self.source_options.clone() else {
            return Ok(());
        };
        let Some(materialized) = materialize_resume_session(&options).await? else {
            return Ok(());
        };
        let options = apply_materialized_options(&options, &materialized);
        self.session_id = options
            .session_id
            .clone()
            .or_else(|| options.resume.clone())
            .unwrap_or_else(|| "default".to_string());
        self.transport = Box::new(SubprocessCLITransport::new(TransportOptions::from(
            &options,
        )));
        self.transcript_mirror = TranscriptMirrorBatcher::from_options(&options);
        self.source_options = Some(options);
        self.materialized_resume = Some(materialized);
        Ok(())
    }

    fn require_connected(&self) -> Result<()> {
        if self.connected && self.initialized {
            Ok(())
        } else {
            Err(CLIConnectionError::new("Not connected. Call connect() first.").into())
        }
    }

    async fn ensure_initialized(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }

        let response = send_control_request_with_callbacks_and_timeout(
            self.transport.as_mut(),
            initialize_request(&self.control_callbacks),
            &self.control_callbacks,
            initialize_timeout_duration(),
        )
        .await?;
        self.initialization_result = Some(response);
        self.initialized = true;
        Ok(())
    }

    pub async fn send_message(&mut self, content: impl Into<String>) -> Result<MessageResponse> {
        self.query(content).await?;
        let messages = self.receive_response().await?;
        let mut content_parts: Vec<String> = Vec::new();
        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut usage: Option<HashMap<String, serde_json::Value>> = None;
        let mut stop_reason: Option<String> = None;
        let mut model = String::new();

        for message in messages {
            match message {
                Message::AssistantMsg {
                    content: assistant_content,
                    ..
                } => {
                    // Track the model from the first assistant message
                    if model.is_empty() {
                        model.clone_from(&assistant_content.model);
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
                    ..
                } => {
                    stop_reason = reason;
                    if let Some(u) = u {
                        usage = Some(u.into_iter().collect());
                    }
                }
                _ => {}
            }
        }

        Ok(MessageResponse {
            content: content_parts.join(""),
            blocks,
            model,
            stop_reason,
            session_id: self.session_id.clone(),
            usage,
        })
    }

    pub async fn query(&mut self, content: impl Into<String>) -> Result<()> {
        self.require_connected()?;
        let content_str = content.into();
        let payload = self.build_user_payload(&content_str, None)?;
        let mut json_payload = serde_json::to_vec(&payload)?;
        json_payload.push(b'\n');
        self.transport.write(&json_payload).await
    }

    pub async fn query_with_session_id(
        &mut self,
        content: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<()> {
        self.require_connected()?;
        let content_str = content.into();
        let session_id = session_id.into();
        let payload = self.build_user_payload(&content_str, Some(&session_id))?;
        let mut json_payload = serde_json::to_vec(&payload)?;
        json_payload.push(b'\n');
        self.transport.write(&json_payload).await
    }

    pub async fn query_stream<S>(&mut self, stream: S) -> Result<()>
    where
        S: Stream<Item = serde_json::Value> + Unpin,
    {
        self.query_stream_with_session_id(stream, "default").await
    }

    pub async fn query_stream_with_session_id<S>(
        &mut self,
        stream: S,
        session_id: impl Into<String>,
    ) -> Result<()>
    where
        S: Stream<Item = serde_json::Value> + Unpin,
    {
        self.require_connected()?;
        self.write_message_stream(stream, &session_id.into()).await
    }

    pub async fn receive_response(&mut self) -> Result<Vec<Message>> {
        self.receive_messages_until(true).await
    }

    pub async fn receive_messages(&mut self) -> Result<Vec<Message>> {
        self.receive_messages_until(false).await
    }

    async fn receive_messages_until(&mut self, stop_at_result: bool) -> Result<Vec<Message>> {
        self.require_connected()?;
        let mut messages = Vec::new();
        while let Some(data) = self.transport.read().await? {
            let line = String::from_utf8_lossy(&data);
            let value = serde_json::from_slice::<serde_json::Value>(&data)?;
            if value.get("type").and_then(|v| v.as_str()) == Some("control_request") {
                respond_to_control_request(
                    self.transport.as_mut(),
                    &value,
                    &self.control_callbacks,
                )
                .await?;
                continue;
            }
            if value.get("type").and_then(|v| v.as_str()) == Some("transcript_mirror") {
                if let Some(batcher) = &mut self.transcript_mirror {
                    messages.extend(batcher.enqueue_value(&value).await?);
                }
                continue;
            }
            let Some(message) = parse_message_line(&line)? else {
                continue;
            };
            let done = matches!(message, Message::ResultMsg { .. });
            if done {
                if let Some(batcher) = &mut self.transcript_mirror {
                    messages.extend(batcher.flush().await?);
                }
            }
            {
                let mut state = self.state.write().await;
                state.messages.push(message.clone());
            }
            messages.push(message);
            if stop_at_result && done {
                break;
            }
        }
        Ok(messages)
    }

    pub async fn stream_message(
        &mut self,
        content: impl Into<String>,
    ) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
        self.require_connected()?;
        let content_str = content.into();
        let payload = self.build_user_payload(&content_str, None)?;
        let json_payload = serde_json::to_vec(&payload)?;
        self.transport.write(&json_payload).await?;
        self.transport
            .write(
                b"
",
            )
            .await?;
        let (tx, rx) = mpsc::unbounded_channel();
        {
            let mut state = self.state.write().await;
            state.is_streaming = true;
        }
        while let Some(data) = self.transport.read().await? {
            let line = String::from_utf8_lossy(&data);
            let value = serde_json::from_slice::<serde_json::Value>(&data)?;
            if value.get("type").and_then(|v| v.as_str()) == Some("control_request") {
                respond_to_control_request(
                    self.transport.as_mut(),
                    &value,
                    &self.control_callbacks,
                )
                .await?;
                continue;
            }
            if value.get("type").and_then(|v| v.as_str()) == Some("transcript_mirror") {
                if let Some(batcher) = &mut self.transcript_mirror {
                    for message in batcher.enqueue_value(&value).await? {
                        let _ = tx.send(StreamEvent::Error(format!("{message:?}")));
                    }
                }
                continue;
            }
            let Some(message) = parse_message_line(&line)? else {
                continue;
            };
            for event in stream_events_from_message(&message, &self.session_id) {
                let _ = tx.send(event);
            }
            let done = matches!(message, Message::ResultMsg { .. });
            if done {
                if let Some(batcher) = &mut self.transcript_mirror {
                    for message in batcher.flush().await? {
                        let _ = tx.send(StreamEvent::Error(format!("{message:?}")));
                    }
                }
            }
            {
                let mut state = self.state.write().await;
                state.messages.push(message);
                if done {
                    state.is_streaming = false;
                }
            }
            if done {
                break;
            }
        }
        Ok(rx)
    }

    async fn write_message_stream<S>(&mut self, mut stream: S, session_id: &str) -> Result<()>
    where
        S: Stream<Item = serde_json::Value> + Unpin,
    {
        while let Some(mut message) = stream.next().await {
            if let Some(object) = message.as_object_mut() {
                object
                    .entry("session_id")
                    .or_insert_with(|| serde_json::Value::String(session_id.to_string()));
            }
            let mut json_payload = serde_json::to_vec(&message)?;
            json_payload.push(b'\n');
            self.transport.write(&json_payload).await?;
        }
        Ok(())
    }

    pub async fn get_conversation_history(&self) -> Result<Vec<Message>> {
        let state = self.state.read().await;
        Ok(state.messages.clone())
    }

    pub async fn abort(&mut self) -> Result<()> {
        if let Some(batcher) = &mut self.transcript_mirror {
            let _ = batcher.flush().await?;
        }
        self.transport.close().await?;
        if let Some(materialized) = &self.materialized_resume {
            materialized.cleanup().await;
        }
        self.materialized_resume = None;
        self.connected = false;
        self.initialized = false;
        Ok(())
    }

    pub async fn disconnect(&mut self) -> Result<()> {
        self.abort().await
    }

    pub async fn close(mut self) -> Result<()> {
        if let Some(batcher) = &mut self.transcript_mirror {
            let _ = batcher.flush().await?;
        }
        self.transport.close().await?;
        if let Some(materialized) = &self.materialized_resume {
            materialized.cleanup().await;
        }
        Ok(())
    }

    pub async fn interrupt(&mut self) -> Result<()> {
        self.require_connected()?;
        send_control_request_with_callbacks(
            self.transport.as_mut(),
            serde_json::json!({"subtype": "interrupt"}),
            &self.control_callbacks,
        )
        .await?;
        Ok(())
    }

    pub async fn set_permission_mode(&mut self, mode: PermissionMode) -> Result<()> {
        self.require_connected()?;
        send_control_request_with_callbacks(
            self.transport.as_mut(),
            serde_json::json!({
                "subtype": "set_permission_mode",
                "mode": mode,
            }),
            &self.control_callbacks,
        )
        .await?;
        Ok(())
    }

    pub async fn set_model(&mut self, model: Option<String>) -> Result<()> {
        self.require_connected()?;
        let model = model.map(serde_json::Value::String);
        send_control_request_with_callbacks(
            self.transport.as_mut(),
            serde_json::json!({
                "subtype": "set_model",
                "model": model.unwrap_or(serde_json::Value::Null),
            }),
            &self.control_callbacks,
        )
        .await?;
        Ok(())
    }

    pub async fn rewind_files(&mut self, user_message_id: impl Into<String>) -> Result<()> {
        self.require_connected()?;
        send_control_request_with_callbacks(
            self.transport.as_mut(),
            serde_json::json!({
                "subtype": "rewind_files",
                "user_message_id": user_message_id.into(),
            }),
            &self.control_callbacks,
        )
        .await?;
        Ok(())
    }

    pub async fn reconnect_mcp_server(&mut self, server_name: impl Into<String>) -> Result<()> {
        self.require_connected()?;
        send_control_request_with_callbacks(
            self.transport.as_mut(),
            serde_json::json!({
                "subtype": "mcp_reconnect",
                "serverName": server_name.into(),
            }),
            &self.control_callbacks,
        )
        .await?;
        Ok(())
    }

    pub async fn toggle_mcp_server(
        &mut self,
        server_name: impl Into<String>,
        enabled: bool,
    ) -> Result<()> {
        self.require_connected()?;
        send_control_request_with_callbacks(
            self.transport.as_mut(),
            serde_json::json!({
                "subtype": "mcp_toggle",
                "serverName": server_name.into(),
                "enabled": enabled,
            }),
            &self.control_callbacks,
        )
        .await?;
        Ok(())
    }

    pub async fn stop_task(&mut self, task_id: impl Into<String>) -> Result<()> {
        self.require_connected()?;
        send_control_request_with_callbacks(
            self.transport.as_mut(),
            serde_json::json!({
                "subtype": "stop_task",
                "task_id": task_id.into(),
            }),
            &self.control_callbacks,
        )
        .await?;
        Ok(())
    }

    pub async fn get_mcp_status(&mut self) -> Result<MCPStatusResponse> {
        self.require_connected()?;
        let response = send_control_request_with_callbacks(
            self.transport.as_mut(),
            serde_json::json!({"subtype": "mcp_status"}),
            &self.control_callbacks,
        )
        .await?;
        let value = serde_json::Value::Object(response);
        Ok(serde_json::from_value(value)?)
    }

    pub async fn get_context_usage(&mut self) -> Result<ContextUsageResponse> {
        self.require_connected()?;
        let response = send_control_request_with_callbacks(
            self.transport.as_mut(),
            serde_json::json!({"subtype": "get_context_usage"}),
            &self.control_callbacks,
        )
        .await?;
        Ok(serde_json::from_value(serde_json::Value::Object(response))?)
    }

    pub fn get_server_info(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        self.initialization_result.as_ref()
    }

    fn build_user_payload(
        &self,
        content: &str,
        session_id: Option<&str>,
    ) -> Result<serde_json::Map<String, serde_json::Value>> {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "type".to_string(),
            serde_json::Value::String("user".to_string()),
        );
        payload.insert(
            "session_id".to_string(),
            serde_json::Value::String(
                session_id
                    .map(String::from)
                    .unwrap_or_else(|| self.session_id.clone()),
            ),
        );
        let message = serde_json::json!({"role": "user", "content": content});
        payload.insert("message".to_string(), message);
        Ok(payload)
    }
}
