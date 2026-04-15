//! Main client for interactive sessions with Claude CLI.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::error::{ClaudeSDKError, Result};
use crate::internal::transport::{SubprocessCLITransport, Transport, TransportOptions};
use crate::types::{ClaudeAgentOptions, ContentBlock, Message, RateLimitInfo};

/// Response from sending a message to Claude
#[derive(Debug, Clone)]
pub struct MessageResponse {
    pub content: String,
    pub blocks: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub session_id: String,
    pub usage: Option<HashMap<String, serde_json::Value>>,
}

/// Events that can occur during streaming
#[derive(Debug, Clone)]
pub enum StreamEvent {
    ContentChunk(String),
    ThinkingChunk { thinking: String, signature: Option<String> },
    ToolUseStart { id: String, name: String, input: serde_json::Map<String, serde_json::Value> },
    ToolUseDelta { id: String, partial_input: String },
    ToolResult { tool_use_id: String, content: Option<serde_json::Value>, is_error: Option<bool> },
    RateLimit(RateLimitInfo),
    Complete(MessageResponse),
    Error(String),
}

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
    options: ClaudeAgentOptions,
    state: Arc<RwLock<ClientState>>,
    session_id: String,
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
        let transport_options = TransportOptions::from(&options);
        let transport = SubprocessCLITransport::new(transport_options);
        let session_id = options.resume.clone().unwrap_or_else(|| "default".to_string());
        let state = Arc::new(RwLock::new(ClientState {
            messages: Vec::new(),
            current_stream_buffer: String::new(),
            is_streaming: false,
            server_info: None,
        }));
        Ok(Self {
            transport: Box::new(transport),
            options,
            state,
            session_id,
        })
    }

    async fn ensure_connected(&mut self) -> Result<()> {
        self.transport.connect().await?;
        Ok(())
    }

    pub async fn send_message(&mut self, content: impl Into<String>) -> Result<MessageResponse> {
        self.ensure_connected().await?;
        let content_str = content.into();
        
        let payload = self.build_user_payload(&content_str)?;
        let json_payload = serde_json::to_vec(&payload)?;
        self.transport.write(&json_payload).await?;
        self.transport.write(b"
").await?;

        let mut content_parts: Vec<String> = Vec::new();
        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut usage: Option<HashMap<String, serde_json::Value>> = None;
        let mut stop_reason: Option<String> = None;
        let mut model = String::new();

        loop {
            match self.transport.read().await? {
                Some(data) => {
                    let line = String::from_utf8_lossy(&data);
                    match serde_json::from_str::<Message>(&line) {
                        Ok(Message::AssistantMsg { content: assistant_content, .. }) => {
                            // Track the model from the first assistant message
                            if model.is_empty() {
                                model.clone_from(&assistant_content.model);
                            }
                            for block in &assistant_content.content {
                                match block {
                                    ContentBlock::Text { text } => content_parts.push(text.clone()),
                                    ContentBlock::Thinking { thinking, .. } => content_parts.push(thinking.clone()),
                                    _ => {}
                                }
                                blocks.push(block.clone());
                            }
                        }
                        Ok(Message::ResultMsg { stop_reason: reason, usage: u, .. }) => {
                            stop_reason = reason;
                            if let Some(u) = u {
                                usage = Some(u.into_iter().map(|(k, v)| (k, v)).collect());
                            }
                            break;
                        }
                        Ok(_) => {}
                        Err(e) => return Err(ClaudeSDKError::Serialization(e)),
                    }
                }
                None => break,
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

    pub async fn stream_message(&mut self, content: impl Into<String>) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
        self.ensure_connected().await?;
        let content_str = content.into();
        let payload = self.build_user_payload(&content_str)?;
        let json_payload = serde_json::to_vec(&payload)?;
        self.transport.write(&json_payload).await?;
        self.transport.write(b"
").await?;
        let (_tx, rx) = mpsc::unbounded_channel();
        { let mut state = self.state.write().await; state.is_streaming = true; }
        Ok(rx)
    }

    pub async fn get_conversation_history(&self) -> Result<Vec<Message>> {
        let state = self.state.read().await;
        Ok(state.messages.clone())
    }

    pub fn abort(&mut self) -> Result<()> {
        let _ = self.transport.close();
        Ok(())
    }

    pub async fn close(mut self) -> Result<()> {
        self.transport.close().await?;
        Ok(())
    }

    fn build_user_payload(&self, content: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
        let mut payload = serde_json::Map::new();
        payload.insert("type".to_string(), serde_json::Value::String("user".to_string()));
        payload.insert("session_id".to_string(), serde_json::Value::String(self.session_id.clone()));
        let message = serde_json::json!({"role": "user", "content": content});
        payload.insert("message".to_string(), message);
        Ok(payload)
    }
}

impl MessageResponse {
    pub fn has_tool_uses(&self) -> bool {
        self.blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }
    pub fn get_tool_uses(&self) -> Vec<&ContentBlock> {
        self.blocks.iter().filter(|b| matches!(b, ContentBlock::ToolUse { .. })).collect()
    }
}
