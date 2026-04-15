use crate::error::{ClaudeSDKError, Result};
use crate::internal::transport::{SubprocessCLITransport, Transport, TransportOptions};
use crate::types::{ClaudeAgentOptions, Message};
use serde::{Deserialize, Serialize};

/// Token usage information from a query response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
}

/// Result from a one-shot query to Claude.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// The text content of the response.
    pub content: String,
    /// Token usage statistics, if available.
    pub usage: Option<TokenUsage>,
    /// The reason the response finished (e.g., "end_turn", "max_tokens").
    pub finish_reason: String,
}

/// Perform a one-shot query to Claude Code.
///
/// This is a convenience function for simple, stateless interactions where you do not
/// need bidirectional communication or conversation management. It creates a temporary
/// client, sends a single prompt, and returns the complete response.
///
/// # Arguments
///
/// * `prompt` - The prompt to send to Claude.
/// * `options` - Optional configuration options. If None, default options are used.
///
/// # Returns
///
/// A `QueryResult` containing the response content, usage statistics, and finish reason.
///
/// # Example
///
/// ```rust
/// use claude_agent_sdk::query;
///
/// async fn example() -> Result<(), Box<dyn std::error::Error>> {
///     let result = query("What is Rust?", None).await?;
///     println!("{}", result.content);
///     Ok(())
/// }
/// ```
pub async fn query(
    prompt: impl Into<String>,
    options: Option<ClaudeAgentOptions>,
) -> Result<QueryResult> {
    let prompt = prompt.into();
    let options = options.unwrap_or_default();

    // Create transport with options
    let transport_options = TransportOptions::from(&options);
    let mut transport = SubprocessCLITransport::new(transport_options);

    // Connect to the CLI
    transport.connect().await?;

    // Build and send the user message
    let user_message = serde_json::json!({
        "type": "user",
        "message": {
            "kind": "text",
            "text": prompt
        }
    });

    transport
        .write(format!("{}\n", user_message).as_bytes())
        .await?;

    // Collect all text content from assistant messages
    let mut content_parts: Vec<String> = Vec::new();
    let mut usage: Option<TokenUsage> = None;
    let mut finish_reason = String::from("unknown");

    // Read messages until we get the result
    loop {
        match transport.read().await? {
            Some(data) => {
                let line = String::from_utf8_lossy(&data);
                match serde_json::from_str::<Message>(&line) {
                    Ok(Message::AssistantMsg { content, .. }) => {
                        // Extract text content from assistant message
                        for block in &content.content {
                            if let crate::types::ContentBlock::Text { text } = block {
                                content_parts.push(text.clone());
                            }
                        }
                    }
                    Ok(Message::ResultMsg {
                        usage: msg_usage,
                        stop_reason,
                        result,
                        ..
                    }) => {
                        // Capture final result data
                        if let Some(result_text) = result {
                            // If no content was collected from assistant messages,
                            // use the result text
                            if content_parts.is_empty() {
                                content_parts.push(result_text);
                            }
                        }

                        // Extract usage from the result message
                        if let Some(u) = msg_usage {
                            usage = extract_token_usage(&u);
                        }

                        // Extract finish reason
                        if let Some(reason) = stop_reason {
                            finish_reason = reason;
                        }

                        break;
                    }
                    Ok(_) => {
                        // Ignore other message types
                    }
                    Err(e) => {
                        return Err(ClaudeSDKError::Serialization(e));
                    }
                }
            }
            None => {
                // End of stream
                break;
            }
        }
    }

    // Close the transport
    transport.close().await?;

    let content = if content_parts.is_empty() {
        String::new()
    } else {
        content_parts.join("")
    };

    Ok(QueryResult {
        content,
        usage,
        finish_reason,
    })
}

/// Extract TokenUsage from a JSON map.
fn extract_token_usage(usage_map: &serde_json::Map<String, serde_json::Value>) -> Option<TokenUsage> {
    let input_tokens = usage_map
        .get("input_tokens")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)?;
    let output_tokens = usage_map
        .get("output_tokens")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)?;
    let total_tokens = usage_map
        .get("total_tokens")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)?;

    Some(TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens,
    })
}
