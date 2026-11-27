use crate::client_types::{MessageResponse, StreamEvent};
use crate::error::error_result_text;
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
                    ContentBlock::ToolUse { id, name, input } => {
                        events.push(StreamEvent::ToolUseStart {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    ContentBlock::ServerToolUse { id, name, input } => {
                        events.push(StreamEvent::ToolUseStart {
                            id: id.clone(),
                            name: name.as_str().to_string(),
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
                // The CLI nests per-turn token usage inside `message.usage`
                // (deserialized into `content.usage`); the top-level
                // `AssistantMsg.usage` is only set by some SDK variants. Prefer
                // the nested usage and fall back to the top-level field so we
                // never drop input/output token counts.
                usage: content
                    .usage
                    .clone()
                    .or_else(|| usage.clone())
                    .map(|usage| usage.into_iter().collect()),
            }));
            events
        }
        Message::RateLimitEventMsg {
            rate_limit_info, ..
        } => vec![StreamEvent::RateLimit(rate_limit_info.clone())],
        Message::StreamEventMsg {
            event: Some(event),
            session_id,
            ..
        } => stream_events_from_raw_event(event, session_id),
        Message::ResultMsg { is_error: true, .. } => vec![StreamEvent::Error(
            // The same text selection as the ResultError raised after an
            // error result (errors, then result, then subtype, then status).
            serde_json::to_value(message)
                .ok()
                .and_then(|value| value.as_object().map(error_result_text))
                .unwrap_or_else(|| "unknown error".to_string()),
        )],
        Message::ResultMsg {
            is_error: false,
            session_id,
            stop_reason,
            usage,
            model_usage,
            ..
        } => vec![StreamEvent::TurnComplete(MessageResponse {
            content: String::new(),
            blocks: Vec::new(),
            model: String::new(),
            stop_reason: stop_reason.clone(),
            session_id: session_id.clone(),
            usage: usage
                .clone()
                .or_else(|| model_usage.clone())
                .map(|usage| usage.into_iter().collect()),
        })],
        Message::ConversationResetMsg(reset) => vec![StreamEvent::ConversationReset(reset.clone())],
        Message::MirrorErrorMsg(mirror) => vec![StreamEvent::Error(mirror.error.clone())],
        _ => Vec::new(),
    }
}

fn stream_events_from_raw_event(
    event: &serde_json::Map<String, serde_json::Value>,
    session_id: &str,
) -> Vec<StreamEvent> {
    match event.get("type").and_then(|value| value.as_str()) {
        Some("content_block_delta") => {
            let Some(delta) = event.get("delta").and_then(|value| value.as_object()) else {
                return Vec::new();
            };
            match delta.get("type").and_then(|value| value.as_str()) {
                Some("text_delta") => delta
                    .get("text")
                    .and_then(|value| value.as_str())
                    .map(|text| vec![StreamEvent::ContentChunk(text.to_string())])
                    .unwrap_or_default(),
                Some("thinking_delta") => delta
                    .get("thinking")
                    .and_then(|value| value.as_str())
                    .map(|thinking| {
                        vec![StreamEvent::ThinkingChunk {
                            thinking: thinking.to_string(),
                            signature: None,
                        }]
                    })
                    .unwrap_or_default(),
                _ => Vec::new(),
            }
        }
        Some("content_block_start") => {
            let Some(block) = event
                .get("content_block")
                .and_then(|value| value.as_object())
            else {
                return Vec::new();
            };
            match block.get("type").and_then(|value| value.as_str()) {
                Some("tool_use" | "server_tool_use") => vec![StreamEvent::ToolUseStart {
                    id: block
                        .get("id")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    input: block
                        .get("input")
                        .and_then(|value| value.as_object())
                        .cloned()
                        .unwrap_or_default(),
                }],
                _ => Vec::new(),
            }
        }
        Some("message_delta") => {
            let usage = event
                .get("usage")
                .and_then(|value| value.as_object())
                .cloned();
            let stop_reason = event
                .get("delta")
                .and_then(|value| value.as_object())
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            usage
                .map(|usage| {
                    vec![StreamEvent::Complete(MessageResponse {
                        content: String::new(),
                        blocks: Vec::new(),
                        model: String::new(),
                        stop_reason,
                        session_id: session_id.to_string(),
                        usage: Some(usage.into_iter().collect()),
                    })]
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    #[test]
    fn assistant_message_complete_carries_nested_usage() {
        // The CLI nests usage inside `message.usage`. The Complete event must
        // surface those token counts so downstream providers can report them.
        let raw = serde_json::json!({
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "model": "claude-sonnet-4-5",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 11, "output_tokens": 7},
                "content": [{"type": "text", "text": "hi"}]
            },
            "session_id": "sess_1",
            "uuid": "assistant-1"
        });
        let message: Message = serde_json::from_value(raw).expect("valid assistant message");

        let events = stream_events_from_message(&message, "fallback");
        let complete = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Complete(response) => Some(response),
                _ => None,
            })
            .expect("expected a Complete event");

        let usage = complete.usage.as_ref().expect("usage should be present");
        assert_eq!(usage.get("input_tokens").and_then(|v| v.as_i64()), Some(11));
        assert_eq!(usage.get("output_tokens").and_then(|v| v.as_i64()), Some(7));
    }

    #[test]
    fn error_result_uses_result_error_text_selection() {
        let message: Message = serde_json::from_value(serde_json::json!({
            "type": "result", "subtype": "success", "is_error": true,
            "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1,
            "session_id": "s", "errors": [], "result": "  API Error: 529 overloaded "
        }))
        .unwrap();
        let events = stream_events_from_message(&message, "fallback");
        assert!(matches!(
            events.as_slice(),
            [StreamEvent::Error(text)] if text == "API Error: 529 overloaded"
        ));
    }

    #[test]
    fn conversation_reset_maps_to_its_own_event() {
        let message = crate::internal::parser::parse_message_value(serde_json::json!({
            "type": "conversation_reset", "new_conversation_id": "c2",
            "uuid": "u", "session_id": "s"
        }))
        .unwrap()
        .unwrap();
        let events = stream_events_from_message(&message, "fallback");
        assert!(matches!(
            events.as_slice(),
            [StreamEvent::ConversationReset(reset)] if reset.new_conversation_id == "c2"
        ));
    }
}
