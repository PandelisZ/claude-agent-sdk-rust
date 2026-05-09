use crate::client_types::{MessageResponse, StreamEvent};
use crate::types::{ContentBlock, Message};

pub(crate) fn stream_events_from_message(
    message: &Message,
    fallback_session_id: &str,
) -> Vec<StreamEvent> {
    match message {
        Message::AssistantMsg {
            content,
            stop_reason,
            session_id,
            usage,
            ..
        } => {
            let mut events = Vec::new();
            for block in &content.content {
                match block {
                    ContentBlock::Text { text } => {
                        events.push(StreamEvent::ContentChunk(text.clone()));
                    }
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } => {
                        events.push(StreamEvent::ThinkingChunk {
                            thinking: thinking.clone(),
                            signature: Some(signature.clone()),
                        });
                    }
                    ContentBlock::ToolUse { id, name, input }
                    | ContentBlock::ServerToolUse { id, name, input } => {
                        events.push(StreamEvent::ToolUseStart {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        events.push(StreamEvent::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: content.clone(),
                            is_error: *is_error,
                        });
                    }
                    ContentBlock::ServerToolResult {
                        tool_use_id,
                        content,
                    } => {
                        events.push(StreamEvent::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: Some(content.clone()),
                            is_error: None,
                        });
                    }
                }
            }
            events.push(StreamEvent::Complete(MessageResponse {
                content: content
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                blocks: content.content.clone(),
                model: content.model.clone(),
                stop_reason: stop_reason.clone().or_else(|| content.stop_reason.clone()),
                session_id: session_id
                    .clone()
                    .unwrap_or_else(|| fallback_session_id.to_string()),
                usage: usage.clone().map(|usage| usage.into_iter().collect()),
            }));
            events
        }
        Message::RateLimitEventMsg {
            rate_limit_info, ..
        } => vec![StreamEvent::RateLimit(rate_limit_info.clone())],
        Message::ResultMsg {
            is_error: true,
            result,
            errors,
            ..
        } => vec![StreamEvent::Error(
            errors
                .as_ref()
                .map(|errors| errors.join("\n"))
                .or_else(|| result.clone())
                .unwrap_or_else(|| "Claude result indicated an error".to_string()),
        )],
        _ => Vec::new(),
    }
}
