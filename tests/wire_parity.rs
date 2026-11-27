use claude_code_sdk_rust::{
    AssistantContent, ContentBlock, Message, TaskUpdatedStatus, UserContent,
};
use serde_json::json;

use claude_code_sdk_rust::internal::parser::parse_message_value;

#[test]
fn deserializes_user_message_from_cli_wire_shape() {
    let raw = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": "hello"
        },
        "parent_tool_use_id": null,
        "session_id": "default",
        "uuid": "user-1"
    });

    let parsed: Message = serde_json::from_value(raw).expect("valid user message");

    match parsed {
        Message::UserMsg {
            content:
                UserContent {
                    role,
                    content: serde_json::Value::String(text),
                },
            uuid,
            parent_tool_use_id,
            ..
        } => {
            assert_eq!(role, Some("user".to_string()));
            assert_eq!(text, "hello");
            assert_eq!(uuid, Some("user-1".to_string()));
            assert_eq!(parent_tool_use_id, None);
        }
        other => panic!("expected user message, got {other:?}"),
    }
}

#[test]
fn deserializes_assistant_message_with_nested_metadata_and_server_tools() {
    let raw = json!({
        "type": "assistant",
        "message": {
            "id": "msg_1",
            "model": "claude-sonnet-4-5",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 2},
            "content": [
                {"type": "text", "text": "hi"},
                {"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {"query": "rust"}},
                {"type": "advisor_tool_result", "tool_use_id": "srv_1", "content": {"type": "web_search_result"}}
            ]
        },
        "session_id": "sess_1",
        "uuid": "assistant-1"
    });

    let parsed: Message = serde_json::from_value(raw).expect("valid assistant message");

    match parsed {
        Message::AssistantMsg {
            content:
                AssistantContent {
                    id,
                    model,
                    stop_reason,
                    usage,
                    content,
                },
            session_id,
            uuid,
            ..
        } => {
            assert_eq!(id, Some("msg_1".to_string()));
            assert_eq!(model, "claude-sonnet-4-5");
            assert_eq!(stop_reason, Some("end_turn".to_string()));
            assert_eq!(
                usage.unwrap().get("output_tokens").and_then(|v| v.as_i64()),
                Some(2)
            );
            assert_eq!(session_id, Some("sess_1".to_string()));
            assert_eq!(uuid, Some("assistant-1".to_string()));
            assert!(matches!(content[0], ContentBlock::Text { .. }));
            assert!(matches!(content[1], ContentBlock::ServerToolUse { .. }));
            assert!(matches!(content[2], ContentBlock::ServerToolResult { .. }));
        }
        other => panic!("expected assistant message, got {other:?}"),
    }
}

#[test]
fn deserializes_result_message_current_fields() {
    let raw = json!({
        "type": "result",
        "subtype": "success",
        "duration_ms": 10,
        "duration_api_ms": 8,
        "is_error": false,
        "num_turns": 1,
        "session_id": "sess_1",
        "stop_reason": "end_turn",
        "modelUsage": {"claude-sonnet-4-5": {"input_tokens": 1}},
        "deferred_tool_use": {"id": "toolu_1", "name": "Bash", "input": {"command": "pwd"}},
        "errors": ["recoverable"],
        "api_error_status": 429,
        "uuid": "result-1"
    });

    let parsed: Message = serde_json::from_value(raw).expect("valid result message");

    match parsed {
        Message::ResultMsg {
            model_usage,
            deferred_tool_use,
            errors,
            api_error_status,
            uuid,
            ..
        } => {
            assert!(model_usage.unwrap().contains_key("claude-sonnet-4-5"));
            assert_eq!(deferred_tool_use.unwrap().name, "Bash");
            assert_eq!(errors, Some(vec!["recoverable".to_string()]));
            assert_eq!(api_error_status, Some(429));
            assert_eq!(uuid, Some("result-1".to_string()));
        }
        other => panic!("expected result message, got {other:?}"),
    }
}

#[test]
fn parser_skips_unknown_message_types_for_forward_compatibility() {
    let raw = json!({
        "type": "future_message",
        "payload": {"kept": true}
    });

    let parsed = parse_message_value(raw).expect("unknown messages are not parse errors");

    assert!(parsed.is_none());
}

#[test]
fn parser_returns_typed_task_system_messages() {
    let raw = json!({
        "type": "system",
        "subtype": "task_progress",
        "task_id": "task-abc",
        "description": "Halfway",
        "usage": {"total_tokens": 123, "tool_uses": 2, "duration_ms": 456},
        "uuid": "uuid-1",
        "session_id": "session-1",
        "tool_use_id": "toolu_1",
        "last_tool_name": "Read"
    });

    let parsed = parse_message_value(raw)
        .expect("valid task message")
        .unwrap();

    match parsed {
        Message::TaskProgressMsg(message) => {
            assert_eq!(message.task_id, "task-abc");
            assert_eq!(message.usage.total_tokens, 123);
            assert_eq!(message.tool_use_id.as_deref(), Some("toolu_1"));
            assert_eq!(message.last_tool_name.as_deref(), Some("Read"));
        }
        other => panic!("expected task progress message, got {other:?}"),
    }
}

#[test]
fn parser_returns_typed_task_updated_message_with_status_from_patch() {
    let raw = json!({
        "type": "system",
        "subtype": "task_updated",
        "task_id": "task-xyz",
        "patch": {"status": "killed", "end_time": 1234},
        "session_id": "session-1",
        "uuid": "uuid-9"
    });

    let parsed = parse_message_value(raw)
        .expect("valid task_updated message")
        .unwrap();

    match parsed {
        Message::TaskUpdatedMsg(message) => {
            assert_eq!(message.task_id, "task-xyz");
            // status is derived from patch.status, not a top-level field.
            assert_eq!(message.status, Some(TaskUpdatedStatus::Killed));
            assert_eq!(message.patch["end_time"], 1234);
            assert_eq!(message.session_id.as_deref(), Some("session-1"));
            assert_eq!(message.uuid.as_deref(), Some("uuid-9"));
            // `killed` is terminal across both lifecycle vocabularies.
            assert!(claude_code_sdk_rust::is_terminal_task_status("killed"));
        }
        other => panic!("expected task_updated message, got {other:?}"),
    }
}

#[test]
fn parser_task_updated_tolerates_missing_status_and_ids() {
    let raw = json!({
        "type": "system",
        "subtype": "task_updated",
        "task_id": "task-xyz",
        "patch": {"end_time": 1234}
    });

    let parsed = parse_message_value(raw)
        .expect("valid task_updated message")
        .unwrap();

    match parsed {
        Message::TaskUpdatedMsg(message) => {
            assert_eq!(message.task_id, "task-xyz");
            assert_eq!(message.status, None);
            assert_eq!(message.session_id, None);
            assert_eq!(message.uuid, None);
        }
        other => panic!("expected task_updated message, got {other:?}"),
    }
}

#[test]
fn parser_returns_typed_hook_system_messages() {
    let raw = json!({
        "type": "system",
        "subtype": "hook_response",
        "hook_event": "PostToolUse",
        "session_id": "session-1",
        "uuid": "uuid-1",
        "output": "",
        "exit_code": 0,
        "outcome": "success"
    });

    let parsed = parse_message_value(raw)
        .expect("valid hook message")
        .unwrap();

    match parsed {
        Message::HookEventMsg(message) => {
            assert_eq!(message.subtype, "hook_response");
            assert_eq!(message.hook_event_name, "PostToolUse");
            assert_eq!(message.session_id.as_deref(), Some("session-1"));
            assert_eq!(message.data["outcome"], "success");
        }
        other => panic!("expected hook event message, got {other:?}"),
    }
}

#[test]
fn parser_returns_typed_mirror_error_messages() {
    let raw = json!({
        "type": "system",
        "subtype": "mirror_error",
        "key": {"project_key": "proj", "session_id": "sess"},
        "error": "disk full"
    });

    let parsed = parse_message_value(raw)
        .expect("valid mirror error")
        .unwrap();

    match parsed {
        Message::MirrorErrorMsg(message) => {
            assert_eq!(message.error, "disk full");
            assert_eq!(
                message.key.as_ref().and_then(|key| key.get("session_id")),
                Some(&json!("sess"))
            );
            assert_eq!(message.data["subtype"], "mirror_error");
        }
        other => panic!("expected mirror error message, got {other:?}"),
    }
}

#[test]
fn parser_accepts_python_rate_limit_wire_values() {
    let raw = json!({
        "type": "rate_limit_event",
        "rate_limit_info": {
            "status": "allowed_warning",
            "resetsAt": 1700000000,
            "rateLimitType": "five_hour",
            "utilization": 0.91
        },
        "uuid": "abc-123",
        "session_id": "session_xyz"
    });

    let parsed = parse_message_value(raw)
        .expect("valid rate limit event")
        .unwrap();

    match parsed {
        Message::RateLimitEventMsg {
            rate_limit_info,
            uuid,
            session_id,
        } => {
            assert_eq!(uuid, "abc-123");
            assert_eq!(session_id, "session_xyz");
            assert_eq!(rate_limit_info.resets_at, Some(1700000000));
            assert_eq!(rate_limit_info.utilization, Some(0.91));
        }
        other => panic!("expected rate limit event, got {other:?}"),
    }
}

#[test]
fn parser_accepts_string_tool_use_result_from_builtin_tool() {
    // The CLI sometimes echoes a tool result as a bare string (e.g. a
    // built-in tool's error text) instead of a structured object. The
    // parser must tolerate this rather than failing with a serde
    // "expected a map" error.
    let raw = json!({
        "type": "user",
        "message": {"role": "user", "content": "ack"},
        "tool_use_result": "Error: File does not exist. Note: your current working directory is /tmp",
        "parent_tool_use_id": "toolu_42"
    });

    let parsed = parse_message_value(raw)
        .expect("string tool_use_result must not be a parse error")
        .unwrap();

    match parsed {
        Message::UserMsg {
            tool_use_result, ..
        } => {
            assert_eq!(
                tool_use_result.and_then(|value| value.as_str().map(str::to_string)),
                Some(
                    "Error: File does not exist. Note: your current working directory is /tmp"
                        .to_string()
                )
            );
        }
        other => panic!("expected user message, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Upstream v0.2.189 message parity
// ---------------------------------------------------------------------------

fn parse(raw: serde_json::Value) -> Message {
    parse_message_value(raw)
        .expect("valid message")
        .expect("known message type")
}

fn parse_err(raw: serde_json::Value) -> String {
    match parse_message_value(raw) {
        Err(claude_code_sdk_rust::ClaudeSDKError::MessageParse(err)) => err.message,
        other => panic!("expected MessageParse error, got {other:?}"),
    }
}

#[test]
fn assistant_unknown_content_blocks_are_dropped_not_fatal() {
    let raw = json!({
        "type": "assistant",
        "message": {
            "id": "msg_1",
            "model": "claude-sonnet-4-5",
            "content": [
                {"type": "redacted_thinking", "data": "abc"},
                {"type": "text", "text": "hi"},
                {"type": "web_search_tool_result", "tool_use_id": "srv_1", "content": []},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}},
                {"type": "some_future_block"}
            ]
        }
    });
    let Message::AssistantMsg { content, .. } = parse(raw.clone()) else {
        panic!("expected assistant");
    };
    assert_eq!(content.content.len(), 2);
    assert!(matches!(&content.content[0], ContentBlock::Text { text } if text == "hi"));
    assert!(matches!(&content.content[1], ContentBlock::ToolUse { id, .. } if id == "t1"));

    // Direct serde deserialization is equally tolerant.
    let direct: Message = serde_json::from_value(raw).expect("serde tolerates unknown blocks");
    let Message::AssistantMsg { content, .. } = direct else {
        panic!("expected assistant");
    };
    assert_eq!(content.content.len(), 2);
}

#[test]
fn assistant_content_validation_matches_python_errors() {
    let base = |content: serde_json::Value| json!({"type": "assistant", "message": {"model": "m", "content": content}});
    assert_eq!(
        parse_err(base(json!(["oops"]))),
        "Invalid content block (expected dict, got str)"
    );
    assert_eq!(
        parse_err(base(json!([{"text": "no type"}]))),
        "Missing required field in assistant message: 'type'"
    );
    assert_eq!(
        parse_err(base(json!("text"))),
        "Invalid assistant content (expected list, got str)"
    );
    assert_eq!(
        parse_err(base(json!([{"type": "thinking", "thinking": "x"}]))),
        "Missing required field in assistant message: 'signature'"
    );
    assert_eq!(
        parse_err(json!({"type": "assistant", "message": {"content": []}})),
        "Missing required field in assistant message: 'model'"
    );
}

#[test]
fn assistant_top_level_metadata_copied_from_nested_message() {
    let raw = json!({
        "type": "assistant",
        "message": {
            "id": "msg_42",
            "model": "claude-sonnet-4-5",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 5},
            "content": [{"type": "text", "text": "hi"}]
        },
        "error": "some_new_error_kind",
        "user_message_uuid": "u-1",
        "user_message_uuids": ["u-0", "u-1"]
    });
    match parse(raw) {
        Message::AssistantMsg {
            message_id,
            stop_reason,
            usage,
            error,
            user_message_uuid,
            user_message_uuids,
            ..
        } => {
            assert_eq!(message_id.as_deref(), Some("msg_42"));
            assert_eq!(stop_reason.as_deref(), Some("end_turn"));
            assert_eq!(usage.unwrap()["output_tokens"], 5);
            assert_eq!(
                error,
                Some(claude_code_sdk_rust::AssistantMessageErrorKind::Other(
                    "some_new_error_kind".to_string()
                ))
            );
            assert_eq!(user_message_uuid.as_deref(), Some("u-1"));
            assert_eq!(
                user_message_uuids,
                Some(vec!["u-0".to_string(), "u-1".to_string()])
            );
        }
        other => panic!("expected assistant, got {other:?}"),
    }
}

#[test]
fn assistant_message_error_known_values_and_alias() {
    let kind: claude_code_sdk_rust::AssistantMessageError =
        serde_json::from_value(json!("rate_limit")).unwrap();
    assert_eq!(
        kind,
        claude_code_sdk_rust::AssistantMessageErrorKind::RateLimit
    );
    assert_eq!(serde_json::to_value(&kind).unwrap(), json!("rate_limit"));
}

#[test]
fn stream_event_carries_user_message_uuids() {
    let raw = json!({
        "type": "stream_event",
        "uuid": "e-1",
        "session_id": "s",
        "event": {"type": "message_start"},
        "user_message_uuid": "u-1",
        "user_message_uuids": ["u-1"]
    });
    match parse(raw) {
        Message::StreamEventMsg {
            user_message_uuid,
            user_message_uuids,
            ..
        } => {
            assert_eq!(user_message_uuid.as_deref(), Some("u-1"));
            assert_eq!(user_message_uuids, Some(vec!["u-1".to_string()]));
        }
        other => panic!("expected stream event, got {other:?}"),
    }
}

#[test]
fn conversation_reset_message_parses_and_requires_fields() {
    let raw = json!({
        "type": "conversation_reset",
        "new_conversation_id": "conv-2",
        "uuid": "r-1",
        "session_id": "old-session"
    });
    match parse(raw.clone()) {
        Message::ConversationResetMsg(reset) => {
            assert_eq!(
                reset,
                claude_code_sdk_rust::ConversationResetMessage {
                    new_conversation_id: "conv-2".to_string(),
                    uuid: "r-1".to_string(),
                    session_id: "old-session".to_string(),
                }
            );
            assert_eq!(
                serde_json::to_value(Message::ConversationResetMsg(reset)).unwrap(),
                raw
            );
        }
        other => panic!("expected conversation reset, got {other:?}"),
    }
    assert_eq!(
        parse_err(json!({"type": "conversation_reset", "uuid": "r", "session_id": "s"})),
        "Missing required field in conversation_reset message: 'new_conversation_id'"
    );
}

#[test]
fn result_message_new_fields_and_lenient_values() {
    let raw = json!({
        "type": "result",
        "subtype": "success",
        "duration_ms": 1500.0,
        "duration_api_ms": 1200,
        "is_error": false,
        "num_turns": 2,
        "session_id": "s",
        "errors": "bare string error",
        "deferred_tool_use": {},
        "terminal_reason": "completed",
        "origin": {"kind": "task-notification", "subkind": "scheduled-trigger", "extraKey": 1},
        "user_message_uuid": "u-2",
        "user_message_uuids": ["u-1", "u-2"]
    });
    match parse(raw) {
        Message::ResultMsg {
            duration_ms,
            errors,
            deferred_tool_use,
            terminal_reason,
            origin,
            user_message_uuid,
            user_message_uuids,
            ..
        } => {
            assert_eq!(duration_ms, 1500);
            assert_eq!(errors, Some(vec!["bare string error".to_string()]));
            assert!(deferred_tool_use.is_none());
            assert_eq!(terminal_reason.as_deref(), Some("completed"));
            let origin = origin.expect("origin");
            assert_eq!(
                origin.kind,
                claude_code_sdk_rust::MessageOriginKind::TaskNotification
            );
            assert_eq!(
                origin.subkind,
                Some(claude_code_sdk_rust::TaskNotificationOriginSubkind::ScheduledTrigger)
            );
            assert_eq!(origin.extra["extraKey"], 1);
            assert_eq!(user_message_uuid.as_deref(), Some("u-2"));
            assert_eq!(user_message_uuids.map(|uuids| uuids.len()), Some(2));
        }
        other => panic!("expected result, got {other:?}"),
    }
}

#[test]
fn result_message_errors_list_drops_non_strings_and_malformed_origin_is_none() {
    let raw = json!({
        "type": "result",
        "subtype": "error_during_execution",
        "duration_ms": 1,
        "duration_api_ms": 1,
        "is_error": true,
        "num_turns": 1,
        "session_id": "s",
        "errors": ["boom", 7, null],
        "origin": {"kind": 5}
    });
    match parse(raw) {
        Message::ResultMsg { errors, origin, .. } => {
            assert_eq!(errors, Some(vec!["boom".to_string()]));
            assert!(origin.is_none());
        }
        other => panic!("expected result, got {other:?}"),
    }
    let raw = json!({
        "type": "result", "subtype": "success", "duration_ms": 1, "duration_api_ms": 1,
        "is_error": false, "num_turns": 1, "session_id": "s", "origin": "human"
    });
    assert!(matches!(
        parse(raw),
        Message::ResultMsg { origin: None, .. }
    ));
}

#[test]
fn result_message_missing_field_uses_python_wording() {
    assert_eq!(
        parse_err(json!({"type": "result", "subtype": "success"})),
        "Missing required field in result message: 'duration_ms'"
    );
    assert_eq!(
        parse_err(json!({
            "type": "result", "subtype": "success", "duration_ms": 1, "duration_api_ms": 1,
            "is_error": false, "num_turns": 1, "session_id": "s",
            "deferred_tool_use": {"id": "t", "name": "Bash"}
        })),
        "Missing required field in result message: 'input'"
    );
}

#[test]
fn model_usage_typed_view() {
    let raw = json!({
        "type": "result", "subtype": "success", "duration_ms": 1, "duration_api_ms": 1,
        "is_error": false, "num_turns": 1, "session_id": "s",
        "modelUsage": {
            "claude-opus-4-7": {
                "inputTokens": 10,
                "outputTokens": 20,
                "cacheReadInputTokens": 1,
                "cacheCreationInputTokens": 2,
                "webSearchRequests": 0,
                "costUSD": 0.25,
                "contextWindow": 200000,
                "maxOutputTokens": 32000,
                "canonicalModel": "claude-opus-4-7",
                "provider": "firstParty",
                "futureKey": true
            }
        }
    });
    let message = parse(raw.clone());
    let usage = message.model_usage_typed().expect("typed model usage");
    let entry = &usage["claude-opus-4-7"];
    assert_eq!(entry.input_tokens, Some(10));
    assert_eq!(entry.cost_usd, Some(0.25));
    assert_eq!(entry.provider.as_deref(), Some("firstParty"));
    assert_eq!(entry.extra["futureKey"], true);
    assert_eq!(
        serde_json::to_value(entry).unwrap(),
        raw["modelUsage"]["claude-opus-4-7"],
        "ModelUsage round-trips with verbatim camelCase keys (costUSD)"
    );
}

#[test]
fn user_message_origin_and_typed_blocks() {
    let raw = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                {"type": "text", "text": "look"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}},
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok", "is_error": false}
            ]
        },
        "origin": {
            "kind": "peer",
            "from": "uds:/tmp/peer.sock",
            "name": "Reviewer",
            "fromSession": "sess-9",
            "senderTaskId": "task-1",
            "body": "hello",
            "verifiedPeerPid": 4242
        }
    });
    let parsed = parse(raw.clone());
    let Message::UserMsg {
        content, origin, ..
    } = &parsed
    else {
        panic!("expected user");
    };
    let origin = origin.as_ref().expect("origin");
    assert_eq!(origin.kind, claude_code_sdk_rust::MessageOriginKind::Peer);
    assert_eq!(origin.from_.as_deref(), Some("uds:/tmp/peer.sock"));
    assert_eq!(origin.verified_peer_pid, Some(4242));
    assert_eq!(
        serde_json::to_value(origin).unwrap(),
        raw["origin"],
        "origin round-trips with verbatim wire keys"
    );

    let blocks = content.blocks().expect("list content");
    assert_eq!(blocks.len(), 2, "image block is dropped like Python");
    assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "look"));
    assert!(
        matches!(&blocks[1], ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "t1")
    );
    assert!(content.text().is_none());

    // Unknown origin kinds are preserved.
    let origin = claude_code_sdk_rust::MessageOrigin::from_value(&json!({"kind": "brand-new"}))
        .expect("string kind");
    assert_eq!(
        origin.kind,
        claude_code_sdk_rust::MessageOriginKind::Other("brand-new".to_string())
    );
}

#[test]
fn user_message_content_validation_matches_python_errors() {
    assert_eq!(
        parse_err(json!({"type": "user", "message": {"content": [1]}})),
        "Invalid content block (expected dict, got int)"
    );
    assert_eq!(
        parse_err(json!({"type": "user", "message": {"content": [
            {"type": "tool_use", "name": "x", "input": {}}
        ]}})),
        "Missing required field in user message: 'id'"
    );
    assert_eq!(
        parse_err(json!({"type": "user", "message": {"role": "user"}})),
        "Missing required field in user message: 'content'"
    );
}

#[test]
fn top_level_message_validation_matches_python() {
    match parse_message_value(json!([1, 2])) {
        Err(claude_code_sdk_rust::ClaudeSDKError::MessageParse(err)) => assert_eq!(
            err.message,
            "Invalid message data type (expected dict, got list)"
        ),
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(
        parse_err(json!({"type": ""})),
        "Message missing 'type' field"
    );
    assert_eq!(
        parse_err(json!({"uuid": "x"})),
        "Message missing 'type' field"
    );
    assert!(parse_message_value(json!({"type": 5})).unwrap().is_none());
}

#[test]
fn system_message_data_keeps_full_raw_payload() {
    let raw = json!({
        "type": "system",
        "subtype": "compact_boundary",
        "uuid": "u",
        "session_id": "s"
    });
    let parsed = parse(raw.clone());
    let (subtype, data) = parsed.as_system().expect("system family");
    assert_eq!(subtype, "compact_boundary");
    assert_eq!(serde_json::Value::Object(data.clone()), raw);
    // Serialization emits `type`/`subtype` once.
    let encoded = serde_json::to_string(&parsed).unwrap();
    assert_eq!(encoded.matches("\"type\"").count(), 1);
    assert_eq!(encoded.matches("\"subtype\"").count(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
        raw
    );
}

#[test]
fn task_messages_keep_subtype_and_raw_data_and_parse_leniently() {
    let started = parse(json!({
        "type": "system", "subtype": "task_started", "task_id": "t1",
        "description": "d", "uuid": "u", "session_id": "s", "prompt": "do it"
    }));
    match &started {
        Message::TaskStartedMsg(message) => {
            assert_eq!(message.subtype, "task_started");
            assert_eq!(message.data["prompt"], "do it");
            assert_eq!(message.data["type"], "system");
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(started.as_system().unwrap().0, "task_started");

    let progress = parse(json!({
        "type": "system", "subtype": "task_progress", "task_id": "t1",
        "description": "d", "uuid": "u", "session_id": "s",
        "usage": {"total_tokens": 5, "cache_tokens": 9}
    }));
    match progress {
        Message::TaskProgressMsg(message) => {
            assert_eq!(message.usage.total_tokens, 5);
            assert_eq!(message.usage.tool_uses, 0);
            assert_eq!(message.usage.extra["cache_tokens"], 9);
            let usage = serde_json::to_value(&message.usage).unwrap();
            assert_eq!(usage["total_tokens"], 5, "TaskUsage serializes snake_case");
        }
        other => panic!("unexpected: {other:?}"),
    }

    let notification = parse(json!({
        "type": "system", "subtype": "task_notification", "task_id": "t1",
        "status": "timed_out", "output_file": "/o", "summary": "s",
        "uuid": "u", "session_id": "s"
    }));
    match notification {
        Message::TaskNotificationMsg(message) => assert_eq!(
            message.status,
            claude_code_sdk_rust::TaskNotificationStatus::Other("timed_out".to_string())
        ),
        other => panic!("unexpected: {other:?}"),
    }

    let updated = parse(json!({
        "type": "system", "subtype": "task_updated", "task_id": "t1",
        "patch": {"status": "archived"}
    }));
    match updated {
        Message::TaskUpdatedMsg(message) => {
            assert_eq!(
                message.status,
                Some(TaskUpdatedStatus::Other("archived".to_string()))
            );
            assert_eq!(message.subtype, "task_updated");
            assert_eq!(message.data["patch"]["status"], "archived");
        }
        other => panic!("unexpected: {other:?}"),
    }

    assert_eq!(
        parse_err(
            json!({"type": "system", "subtype": "task_started", "description": "d",
            "uuid": "u", "session_id": "s"})
        ),
        "Missing required field in system message: 'task_id'"
    );
    assert_eq!(
        parse_err(json!({"type": "system"})),
        "Missing required field in system message: 'subtype'"
    );
}

#[test]
fn hook_event_name_falls_back_through_all_keys() {
    let hook = |extra: serde_json::Value| {
        let mut raw = json!({"type": "system", "subtype": "hook_started"});
        raw.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        match parse(raw) {
            Message::HookEventMsg(message) => message,
            other => panic!("unexpected: {other:?}"),
        }
    };
    assert_eq!(
        hook(json!({"hook_event": "", "hook_name": null, "hook_event_name": "Stop"}))
            .hook_event_name,
        "Stop"
    );
    assert_eq!(
        hook(json!({"hook_name": "PreToolUse"})).hook_event_name,
        "PreToolUse"
    );
    let message = hook(json!({}));
    assert_eq!(message.hook_event_name, "");
    assert_eq!(message.data["type"], "system", "data keeps the raw type");
}

#[test]
fn mirror_error_exposes_typed_session_key_and_raw_data() {
    let parsed = parse(json!({
        "type": "system",
        "subtype": "mirror_error",
        "key": {"project_key": "proj", "session_id": "sess", "subpath": "subagents/a"},
        "error": "disk full"
    }));
    match &parsed {
        Message::MirrorErrorMsg(message) => {
            let key = message.session_key().expect("typed key");
            assert_eq!(key.project_key, "proj");
            assert_eq!(key.subpath.as_deref(), Some("subagents/a"));
            assert_eq!(message.data["type"], "system");
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(parsed.as_system().unwrap().0, "mirror_error");
}

#[test]
fn system_init_data_typed_view() {
    let raw = json!({
        "type": "system",
        "subtype": "init",
        "uuid": "u",
        "session_id": "s",
        "apiKeySource": "none",
        "claude_code_version": "2.1.205",
        "cwd": "/work",
        "tools": ["Bash", "Read"],
        "mcp_servers": [{"name": "docs", "status": "connected", "source": "sdk"}],
        "model": "claude-opus-4-7",
        "permissionMode": "default",
        "slash_commands": ["compact"],
        "output_style": "default",
        "skills": [],
        "plugins": [{"name": "p", "path": "/p", "version": "1.0.0"}],
        "plugin_errors": [
            {"plugin": "inline[0]", "type": "path-not-found", "message": "missing", "path": "/x"}
        ],
        "fast_mode_state": "cooldown",
        "fast_mode_disabled_reason": "brand_new_reason",
        "capabilities": ["interrupt_receipt_v1"]
    });
    let parsed = parse(raw.clone());
    let init = parsed.as_system_init().expect("init data");
    assert_eq!(init.r#type.as_deref(), Some("system"));
    assert_eq!(init.subtype.as_deref(), Some("init"));
    assert_eq!(init.api_key_source.as_deref(), Some("none"));
    assert_eq!(
        init.permission_mode,
        Some(claude_code_sdk_rust::PermissionMode::Default)
    );
    assert_eq!(
        init.mcp_servers.as_ref().unwrap()[0].source.as_deref(),
        Some("sdk")
    );
    assert_eq!(
        init.plugin_errors.as_ref().unwrap()[0].r#type,
        "path-not-found"
    );
    assert_eq!(
        init.fast_mode_state,
        Some(claude_code_sdk_rust::FastModeState::Cooldown)
    );
    assert_eq!(
        init.fast_mode_disabled_reason,
        Some(claude_code_sdk_rust::FastModeDisabledReason::Other(
            "brand_new_reason".to_string()
        ))
    );
    assert_eq!(serde_json::to_value(&init).unwrap(), raw);
    assert!(parse(json!({"type": "system", "subtype": "other"}))
        .as_system_init()
        .is_none());
}

#[test]
fn rate_limit_info_raw_and_forward_compatible_values() {
    let info = json!({
        "status": "throttled",
        "rateLimitType": "one_day",
        "overageStatus": "allowed_warning",
        "isUsingOverage": true
    });
    let parsed = parse(json!({
        "type": "rate_limit_event",
        "rate_limit_info": info.clone(),
        "uuid": "u",
        "session_id": "s"
    }));
    match parsed {
        Message::RateLimitEventMsg {
            rate_limit_info, ..
        } => {
            assert_eq!(
                rate_limit_info.status,
                claude_code_sdk_rust::RateLimitStatus::Other("throttled".to_string())
            );
            assert_eq!(
                rate_limit_info.rate_limit_type,
                Some(claude_code_sdk_rust::RateLimitType::Other(
                    "one_day".to_string()
                ))
            );
            assert_eq!(serde_json::Value::Object(rate_limit_info.raw.clone()), info);
            let encoded = serde_json::to_value(&rate_limit_info).unwrap();
            assert_eq!(encoded["overageStatus"], "allowed_warning");
            assert!(encoded.get("raw").is_none());
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(
        serde_json::to_value(claude_code_sdk_rust::RateLimitStatus::AllowedWarning).unwrap(),
        json!("allowed_warning")
    );
    assert_eq!(
        parse_err(json!({
            "type": "rate_limit_event", "rate_limit_info": {}, "uuid": "u", "session_id": "s"
        })),
        "Missing required field in rate_limit_event message: 'status'"
    );
}

#[test]
fn task_usage_tolerates_null_float_and_non_object_values() {
    let progress = parse_message_value(json!({
        "type": "system", "subtype": "task_progress", "task_id": "t1",
        "description": "d", "uuid": "u", "session_id": "s",
        "usage": {"total_tokens": null, "tool_uses": 3.0, "duration_ms": "x"}
    }))
    .expect("null/float counters must not fail the message")
    .expect("a message");
    match progress {
        Message::TaskProgressMsg(message) => {
            assert_eq!(message.usage.total_tokens, 0);
            assert_eq!(message.usage.tool_uses, 3);
            assert_eq!(message.usage.duration_ms, 0);
        }
        other => panic!("unexpected: {other:?}"),
    }

    let progress = parse_message_value(json!({
        "type": "system", "subtype": "task_progress", "task_id": "t1",
        "description": "d", "uuid": "u", "session_id": "s", "usage": null
    }))
    .expect("a non-object usage must not fail the message")
    .expect("a message");
    assert!(matches!(progress, Message::TaskProgressMsg(_)));

    let err = parse_message_value(json!({
        "type": "system", "subtype": "task_progress", "task_id": "t1",
        "description": "d", "uuid": "u", "session_id": "s"
    }))
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Missing required field in system message: 'usage'"
    );
}
