mod support;

use claude_code_sdk_rust::session_store::{InMemorySessionStore, SessionKey, SessionStore};
use claude_code_sdk_rust::{
    ClaudeAgentClient, ClaudeAgentOptions, ClaudeSDKError, HookCallback, HookMatcher, Message,
    PermissionResult, StreamEvent,
};
use futures::{StreamExt, TryStreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use support::{
    assistant, error_result_frame, result_frame, session_state, MockTransport, ReadItem,
};

fn make_client(options: ClaudeAgentOptions, transport: &MockTransport) -> ClaudeAgentClient {
    ClaudeAgentClient::with_transport(options, transport.boxed()).expect("client")
}

#[tokio::test]
async fn client_consumes_transcript_mirror_frames_without_yielding_them() {
    let transport = MockTransport::new();
    let store = InMemorySessionStore::new();
    let temp = std::env::temp_dir().join(format!(
        "claude-rust-client-mirror-test-{}",
        uuid::Uuid::new_v4()
    ));
    let projects_dir = temp.join("projects");
    transport.push(serde_json::json!({
        "type": "transcript_mirror",
        "filePath": projects_dir.join("proj/session-1.jsonl"),
        "entries": [{
            "type": "user",
            "uuid": "entry-1",
            "message": {"content": "stored prompt"}
        }]
    }));
    transport.push(assistant("hello"));
    transport.push(result_frame());

    let mut env = std::collections::HashMap::new();
    env.insert(
        "CLAUDE_CONFIG_DIR".to_string(),
        temp.to_string_lossy().to_string(),
    );
    let options = ClaudeAgentOptions::builder()
        .env(env)
        .session_store(store.clone())
        .build();
    let mut client = make_client(options, &transport);

    client.connect().await.unwrap();
    client.query("hi").await.unwrap();
    let messages = client.collect_response().await.unwrap();

    assert_eq!(messages.len(), 2);
    let entries = store
        .load(SessionKey {
            project_key: "proj".to_string(),
            session_id: "session-1".to_string(),
            subpath: None,
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["uuid"], "entry-1");
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn client_connects_initializes_sends_query_and_receives_response() {
    let transport = MockTransport::new();
    transport.push(assistant("hello"));
    transport.push(result_frame());

    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.expect("connect");
    client.query("hi").await.expect("query");
    let messages = client.collect_response().await.expect("messages");

    assert_eq!(messages.len(), 2);
    assert!(matches!(messages[0], Message::AssistantMsg { .. }));
    assert!(matches!(messages[1], Message::ResultMsg { .. }));

    let state = transport.lock();
    assert_eq!(state.connected, 1);
    assert_eq!(state.writes[0]["type"], "control_request");
    assert_eq!(state.writes[0]["request"]["subtype"], "initialize");
    // Python ClaudeSDKClient.query(str): exact frame and key order.
    assert_eq!(
        serde_json::to_string(&state.writes[1]).unwrap(),
        r#"{"type":"user","message":{"role":"user","content":"hi"},"parent_tool_use_id":null,"session_id":"default"}"#
    );
}

#[tokio::test]
async fn client_prompts_use_default_session_not_options_session() {
    let transport = MockTransport::new();
    let options = ClaudeAgentOptions::builder()
        .session_id("11111111-1111-4111-8111-111111111111")
        .build();
    let mut client = make_client(options, &transport);

    client.connect_with_prompt("first").await.expect("connect");
    client.query("second").await.expect("query");
    client
        .query_with_session_id("third", "custom-session")
        .await
        .expect("query");

    let users = transport.lock().writes_of("user");
    assert_eq!(users[0]["session_id"], "default");
    assert_eq!(users[0]["message"]["content"], "first");
    assert!(users[0]["parent_tool_use_id"].is_null());
    assert_eq!(users[1]["session_id"], "default");
    assert_eq!(users[2]["session_id"], "custom-session");
}

#[tokio::test]
async fn client_query_stream_sends_prompt_frames_and_defaults_session_id() {
    let transport = MockTransport::new();
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);

    client.connect().await.expect("connect");
    client
        .query_stream(futures::stream::iter([
            serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": "first"}
            }),
            serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": "second"},
                "session_id": "custom"
            }),
        ]))
        .await
        .expect("query stream");

    let users = transport.lock().writes_of("user");
    assert_eq!(users[0]["message"]["content"], "first");
    assert_eq!(users[0]["session_id"], "default");
    assert_eq!(users[1]["message"]["content"], "second");
    assert_eq!(users[1]["session_id"], "custom");
}

#[tokio::test]
async fn verbatim_prompts_stamps_every_outgoing_user_message() {
    let transport = MockTransport::new();
    let options = ClaudeAgentOptions::builder().verbatim_prompts(true).build();
    let mut client = make_client(options, &transport);

    client.connect_with_prompt("a").await.unwrap();
    client.query("b").await.unwrap();
    client
        .query_stream(futures::stream::iter([serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "c"},
            "client_composed": false
        })]))
        .await
        .unwrap();

    let users = transport.lock().writes_of("user");
    assert_eq!(users.len(), 3);
    for user in &users {
        assert_eq!(user["client_composed"], true, "{user}");
    }
}

#[tokio::test]
async fn client_query_requires_explicit_connect_like_python_sdk() {
    let client = make_client(ClaudeAgentOptions::default(), &MockTransport::new());

    let err = client.query("hi").await.expect_err("not connected");
    assert!(err.to_string().contains("Not connected"));
    let err = client.get_server_info().expect_err("not connected");
    assert!(err.is_connection_error());
    let err = client
        .receive_response()
        .next()
        .await
        .unwrap()
        .expect_err("not connected");
    assert!(err.to_string().contains("Not connected"));
}

#[tokio::test]
async fn get_server_info_returns_initialize_response_and_clears_on_disconnect() {
    let transport = MockTransport::new();
    transport.lock().init_response = Some(serde_json::json!({"commands": [], "output_style": "x"}));
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.unwrap();
    let info = client.get_server_info().unwrap().unwrap();
    assert_eq!(info["output_style"], "x");
    client.disconnect().await.unwrap();
    assert!(client.get_server_info().is_err());
}

#[tokio::test]
async fn client_receive_messages_drains_without_stopping_at_result() {
    let transport = MockTransport::new();
    transport.push(assistant("first"));
    transport.push(result_frame());
    transport.push(assistant("after result"));
    transport.lock().eof_when_drained = true;

    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.expect("connect");

    let messages = client.collect_messages().await.expect("messages");

    assert_eq!(messages.len(), 3);
    assert!(matches!(messages[0], Message::AssistantMsg { .. }));
    assert!(matches!(messages[1], Message::ResultMsg { .. }));
    assert!(matches!(messages[2], Message::AssistantMsg { .. }));
    assert_eq!(client.get_conversation_history().await.unwrap().len(), 3);
}

#[tokio::test]
async fn receive_response_streams_messages_as_they_arrive() {
    let transport = MockTransport::new();
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.unwrap();
    client.query("hi").await.unwrap();

    let mut stream = client.receive_response();
    transport.push(assistant("one"));
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("first message before the turn ends")
        .unwrap()
        .unwrap();
    assert!(matches!(first, Message::AssistantMsg { .. }));
    // The turn is still running: nothing else is ready.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .is_err()
    );
    transport.push(result_frame());
    let last = stream.next().await.unwrap().unwrap();
    assert!(matches!(last, Message::ResultMsg { .. }));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn client_connect_with_prompt_sends_initial_user_message() {
    let transport = MockTransport::new();
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);

    client.connect_with_prompt("hello").await.expect("connect");

    let state = transport.lock();
    assert_eq!(state.writes[0]["type"], "control_request");
    assert_eq!(state.writes[0]["request"]["subtype"], "initialize");
    assert_eq!(state.writes[1]["type"], "user");
    assert_eq!(state.writes[1]["message"]["content"], "hello");
    assert_eq!(state.writes[1]["session_id"], "default");
}

#[tokio::test]
async fn client_disconnect_closes_transport_and_allows_reconnect() {
    let transport = MockTransport::new();
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);

    client.connect().await.expect("connect");
    client.disconnect().await.expect("disconnect");
    assert!(transport.lock().closed);

    // A custom transport is handed back on disconnect and reused.
    transport.lock().closed = false;
    transport.lock().init_response = Some(serde_json::json!({}));
    client.connect().await.expect("reconnect");
    assert_eq!(transport.lock().connected, 2);
    client.close().await.unwrap();
}

#[tokio::test]
async fn dynamic_control_methods_send_python_compatible_requests() {
    let transport = MockTransport::new();
    transport.lock().auto_control_responses = true;
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);

    client.connect().await.expect("connect");
    client
        .set_permission_mode(claude_code_sdk_rust::PermissionMode::AcceptEdits)
        .await
        .expect("set permission mode");
    client
        .set_model(Some("claude-haiku-4-5-20251001".to_string()))
        .await
        .expect("set model");
    client.set_model(None).await.expect("clear model");
    client.interrupt().await.expect("interrupt");

    let writes = transport.lock().writes.clone();
    assert_eq!(writes[1]["request"]["subtype"], "set_permission_mode");
    assert_eq!(writes[1]["request"]["mode"], "acceptEdits");
    assert_eq!(writes[2]["request"]["subtype"], "set_model");
    assert_eq!(writes[2]["request"]["model"], "claude-haiku-4-5-20251001");
    assert_eq!(writes[3]["request"]["subtype"], "set_model");
    assert!(writes[3]["request"]["model"].is_null());
    assert_eq!(writes[4]["request"]["subtype"], "interrupt");
    // Python request ids: req_{counter}_{8 hex}.
    let id = writes[4]["request_id"].as_str().unwrap();
    assert!(id.starts_with("req_5_"), "{id}");
    assert_eq!(id.len(), "req_5_".len() + 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_request_mid_turn_does_not_drop_interleaved_messages() {
    let transport = MockTransport::new();
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.unwrap();
    client.query("hi").await.unwrap();

    // The turn's messages arrive before the control response.
    transport.push(assistant("before interrupt"));
    transport.push(result_frame());
    let interrupt = client.interrupt();
    let answer = async {
        transport
            .wait_until("interrupt request", |s| {
                s.writes
                    .iter()
                    .any(|w| w["request"]["subtype"] == "interrupt")
            })
            .await;
        let id = transport
            .lock()
            .writes
            .iter()
            .find(|w| w["request"]["subtype"] == "interrupt")
            .unwrap()["request_id"]
            .as_str()
            .unwrap()
            .to_string();
        transport.push(serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": id, "response": {}}
        }));
    };
    let (interrupted, ()) = tokio::join!(interrupt, answer);
    interrupted.unwrap();

    let messages = client.collect_response().await.unwrap();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(matches!(messages[1], Message::ResultMsg { .. }));
}

#[tokio::test]
async fn control_error_response_fails_the_request_with_its_text() {
    let transport = MockTransport::new();
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.unwrap();
    let request = client.stop_task("task-1");
    let answer = async {
        transport
            .wait_until("stop_task", |s| s.writes.len() >= 2)
            .await;
        let id = transport.lock().writes[1]["request_id"]
            .as_str()
            .unwrap()
            .to_string();
        transport.push(serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": id, "error": "no such task"}
        }));
    };
    let (result, ()) = tokio::join!(request, answer);
    match result.unwrap_err() {
        ClaudeSDKError::ControlRequest { subtype, message } => {
            assert_eq!(subtype, "stop_task");
            assert_eq!(message, "no such task");
        }
        other => panic!("unexpected {other:?}"),
    }
}

fn can_use_tool_request(id: &str, tool: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "control_request",
        "request_id": id,
        "request": {
            "subtype": "can_use_tool",
            "tool_name": tool,
            "input": {"command": "pwd"},
            "permission_suggestions": [],
            "tool_use_id": "toolu_1"
        }
    })
}

#[tokio::test]
async fn control_requests_are_answered_while_the_caller_is_not_reading() {
    let transport = MockTransport::new();
    let options = ClaudeAgentOptions::builder()
        .can_use_tool(|tool_name, input, context| async move {
            assert_eq!(tool_name, "Bash");
            assert_eq!(input["command"], "pwd");
            assert_eq!(context.tool_use_id.as_deref(), Some("toolu_1"));
            Ok(PermissionResult::allow())
        })
        .build();
    let mut client = make_client(options, &transport);
    client.connect().await.unwrap();
    client.query("hi").await.unwrap();

    transport.push(can_use_tool_request("permission_1", "Bash"));
    transport
        .wait_until("permission response", |s| {
            s.response_for("permission_1").is_some()
        })
        .await;
    let response = transport.lock().response_for("permission_1").unwrap();
    assert_eq!(
        response,
        serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": "permission_1",
                "response": {"behavior": "allow", "updatedInput": {"command": "pwd"}}
            }
        })
    );
}

#[tokio::test]
async fn control_handlers_run_concurrently() {
    // Hook A only finishes once hook B ran: answered one at a time, A would
    // block B (and the reader) forever.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let rx = Arc::new(tokio::sync::Mutex::new(Some(rx)));
    let tx = Arc::new(std::sync::Mutex::new(Some(tx)));
    let options = ClaudeAgentOptions::builder()
        .hook(
            "PreToolUse",
            HookMatcher::new(HookCallback::new(move |_, _, _| {
                let rx = rx.clone();
                async move {
                    let rx = rx.lock().await.take().unwrap();
                    rx.await.unwrap();
                    Ok(serde_json::json!({"continue_": true}))
                }
            })),
        )
        .hook(
            "PostToolUse",
            HookMatcher::new(HookCallback::new(move |_, _, _| {
                let tx = tx.clone();
                async move {
                    tx.lock().unwrap().take().unwrap().send(()).unwrap();
                    Ok(serde_json::json!({}))
                }
            })),
        )
        .build();
    let transport = MockTransport::new();
    let mut client = make_client(options, &transport);
    client.connect().await.unwrap();
    // Events are registered in sorted order: PostToolUse -> hook_0,
    // PreToolUse -> hook_1.
    transport.push(serde_json::json!({
        "type": "control_request", "request_id": "slow",
        "request": {"subtype": "hook_callback", "callback_id": "hook_1", "input": {}}
    }));
    transport.push(serde_json::json!({
        "type": "control_request", "request_id": "fast",
        "request": {"subtype": "hook_callback", "callback_id": "hook_0", "input": {}}
    }));
    transport
        .wait_until("both hook responses", |s| {
            s.response_for("slow").is_some() && s.response_for("fast").is_some()
        })
        .await;
    let state = transport.lock();
    assert_eq!(
        state.response_for("slow").unwrap()["response"]["response"],
        serde_json::json!({"continue": true})
    );
    // The fast one answered first.
    let order: Vec<_> = state
        .writes
        .iter()
        .filter_map(|w| w["response"]["request_id"].as_str())
        .collect();
    assert_eq!(order, vec!["fast", "slow"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_cancel_request_aborts_the_handler_without_a_response() {
    struct Flag(Arc<AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let (dropped_in, started_in) = (dropped.clone(), started.clone());
    let options = ClaudeAgentOptions::builder()
        .can_use_tool(move |_, _, _| {
            let flag = Flag(dropped_in.clone());
            started_in.store(true, Ordering::SeqCst);
            async move {
                let _flag = flag;
                futures::future::pending::<()>().await;
                Ok(PermissionResult::allow())
            }
        })
        .build();
    let transport = MockTransport::new();
    let mut client = make_client(options, &transport);
    client.connect().await.unwrap();

    transport.push(can_use_tool_request("perm_cancel", "Bash"));
    let started_check = started.clone();
    transport
        .wait_until("callback started", move |_| {
            started_check.load(Ordering::SeqCst)
        })
        .await;
    transport
        .push(serde_json::json!({"type": "control_cancel_request", "request_id": "perm_cancel"}));
    let dropped_check = dropped.clone();
    transport
        .wait_until("callback cancelled", move |_| {
            dropped_check.load(Ordering::SeqCst)
        })
        .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(transport.lock().response_for("perm_cancel").is_none());
}

#[tokio::test]
async fn control_handler_errors_send_python_error_text() {
    let transport = MockTransport::new();
    let options = ClaudeAgentOptions::builder()
        .hook(
            "Stop",
            HookMatcher::new(HookCallback::new(|_, _, _| async {
                Ok(serde_json::Value::Null)
            })),
        )
        .build();
    let mut client = make_client(options, &transport);
    client.connect().await.unwrap();
    transport.push(serde_json::json!({
        "type": "control_request", "request_id": "missing",
        "request": {"subtype": "hook_callback", "callback_id": "hook_9", "input": {}}
    }));
    transport.push(serde_json::json!({
        "type": "control_request", "request_id": "null_output",
        "request": {"subtype": "hook_callback", "callback_id": "hook_0", "input": {}}
    }));
    transport.push(can_use_tool_request("no_callback", "Bash"));
    transport
        .wait_until("three responses", |s| {
            s.writes_of("control_response").len() == 3
        })
        .await;
    let state = transport.lock();
    let error = |id: &str| state.response_for(id).unwrap()["response"]["error"].clone();
    assert_eq!(error("missing"), "No hook callback found for ID: hook_9");
    assert_eq!(
        error("null_output"),
        "hook callback must return a JSON object"
    );
    assert_eq!(error("no_callback"), "canUseTool callback is not provided");
}

#[tokio::test]
async fn client_stream_message_returns_before_the_turn_ends() {
    let transport = MockTransport::new();
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.expect("connect");

    let mut events = client.stream_message("hi").await.expect("stream");
    transport.push(serde_json::json!({
        "type": "assistant",
        "message": {
            "model": "claude-sonnet-4-5",
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "Cargo.toml"}}
            ]
        },
        "session_id": "default"
    }));
    let first = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(first, StreamEvent::ContentChunk(text) if text == "hello"));
    transport.push(result_frame());

    let mut collected = Vec::new();
    while let Some(event) = events.recv().await {
        collected.push(event);
    }
    assert!(collected.iter().any(|event| matches!(
        event,
        StreamEvent::ToolUseStart { id, name, .. } if id == "toolu_1" && name == "Read"
    )));
    assert!(collected
        .iter()
        .any(|event| matches!(event, StreamEvent::Complete(_))));
    assert!(matches!(
        collected.last(),
        Some(StreamEvent::TurnComplete(_))
    ));
}

#[tokio::test]
async fn client_stream_message_answers_can_use_tool_control_requests() {
    let transport = MockTransport::new();
    transport.push(can_use_tool_request("permission_stream", "Read"));
    transport.push(assistant("ok"));
    transport.push(result_frame());

    let options = ClaudeAgentOptions::builder()
        .can_use_tool(|tool_name, _, context| async move {
            assert_eq!(tool_name, "Read");
            assert_eq!(context.tool_use_id.as_deref(), Some("toolu_1"));
            Ok(PermissionResult::allow())
        })
        .build();
    let mut client = make_client(options, &transport);
    client.connect().await.expect("connect");
    let mut events = client.stream_message("hi").await.expect("stream");
    let mut collected = Vec::new();
    while let Some(event) = events.recv().await {
        collected.push(event);
    }
    assert!(collected
        .iter()
        .any(|event| matches!(event, StreamEvent::Complete(_))));
    let response = transport.lock().response_for("permission_stream").unwrap();
    assert_eq!(response["response"]["response"]["behavior"], "allow");
}

#[tokio::test]
async fn sdk_host_only_session_state_frames_are_not_yielded() {
    let transport = MockTransport::new();
    transport.push(session_state("running", true));
    transport.push(assistant("hi"));
    transport.push(session_state("idle", false));
    transport.push(result_frame());
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.unwrap();
    let messages = client.collect_response().await.unwrap();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert!(matches!(messages[0], Message::AssistantMsg { .. }));
    // The caller opted into this one (no sdk_host_only flag).
    assert!(matches!(messages[1], Message::SystemMsg { .. }));
}

#[tokio::test]
async fn malformed_known_message_is_yielded_as_error_and_the_stream_goes_on() {
    let transport = MockTransport::new();
    transport.push(serde_json::json!({"type": "assistant", "message": {"content": []}}));
    transport.push(serde_json::json!({"type": "brand_new_type"}));
    transport.push(result_frame());
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.unwrap();
    let items: Vec<_> = client.receive_response().collect().await;
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], Err(ClaudeSDKError::MessageParse(_))));
    assert!(matches!(&items[1], Ok(Message::ResultMsg { .. })));
}

#[tokio::test]
async fn process_error_after_error_result_becomes_result_error() {
    let transport = MockTransport::new();
    transport.push(error_result_frame());
    transport.push(session_state("idle", true));
    transport.push_item(ReadItem::ProcessExit(1));
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.unwrap();
    let items: Vec<_> = client.receive_messages().collect().await;
    assert_eq!(items.len(), 2);
    assert!(matches!(
        &items[0],
        Ok(Message::ResultMsg { is_error: true, .. })
    ));
    let error = items[1].as_ref().unwrap_err();
    assert!(error.is_process_error());
    assert_eq!(error.exit_code(), Some(1));
    let result_error = error.as_result_error().expect("ResultError");
    assert_eq!(
        result_error.message,
        "Claude Code returned an error result: Reached maximum number of turns (3)"
    );
    assert_eq!(result_error.subtype.as_deref(), Some("error_max_turns"));
    assert_eq!(result_error.terminal_reason.as_deref(), Some("max_turns"));
    assert_eq!(result_error.session_id.as_deref(), Some("sess-err"));
    // Later control requests fail fast instead of timing out.
    let err = tokio::time::timeout(Duration::from_secs(5), client.interrupt())
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.is_connection_error(), "{err}");
}

#[tokio::test]
async fn pending_initialize_fails_with_the_result_error() {
    let transport = MockTransport::new();
    transport.lock().init_response = None;
    transport.push(error_result_frame());
    transport.push_item(ReadItem::ProcessExit(1));
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    let err = client.connect().await.unwrap_err();
    assert!(err.as_result_error().is_some(), "{err:?}");
    // The failed connect tore the session down.
    assert!(transport.lock().closed);
    assert!(client.get_server_info().is_err());
}

fn blocking_hook_options() -> ClaudeAgentOptions {
    ClaudeAgentOptions::builder()
        .hook(
            "PreToolUse",
            HookMatcher::new(HookCallback::new(|_, _, _| async {
                Ok(serde_json::json!({}))
            }))
            .matcher("Bash")
            .on_failure("block"),
        )
        .build()
}

#[tokio::test]
async fn blocking_hook_requires_cli_support_and_tears_down_on_failure() {
    let transport = MockTransport::new();
    let mut client = make_client(blocking_hook_options(), &transport);
    let err = client.connect().await.unwrap_err();
    assert!(err.is_connection_error());
    assert!(err
        .to_string()
        .starts_with("The process answering initialize does not honor on_failure"));
    let state = transport.lock();
    assert!(state.closed);
    assert_eq!(
        state.writes[0]["request"]["hooks"]["PreToolUse"][0],
        serde_json::json!({"matcher": "Bash", "hookCallbackIds": ["hook_0"], "onFailure": "block"})
    );
}

#[tokio::test]
async fn blocking_hook_fails_when_hooks_were_not_applied() {
    let transport = MockTransport::new();
    transport.lock().init_response =
        Some(serde_json::json!({"hook_on_failure_supported": true, "hooks_applied": false}));
    let mut client = make_client(blocking_hook_options(), &transport);
    let err = client.connect().await.unwrap_err();
    assert!(err
        .to_string()
        .starts_with("This session was already running and did not register your hooks"));

    let transport = MockTransport::new();
    transport.lock().init_response = Some(serde_json::json!({"hook_on_failure_supported": true}));
    let mut client = make_client(blocking_hook_options(), &transport);
    client.connect().await.expect("supported and applied");
}

#[tokio::test]
async fn custom_transport_runs_can_use_tool_validation() {
    let options = ClaudeAgentOptions::builder()
        .can_use_tool(|_, _, _| async { Ok(PermissionResult::allow()) })
        .permission_prompt_tool_name("custom")
        .build();
    let err = ClaudeAgentClient::with_transport(options, MockTransport::new().boxed())
        .expect_err("conflicting options");
    assert!(err
        .to_string()
        .contains("can_use_tool callback cannot be used"));
}

#[tokio::test]
async fn connect_with_stream_writes_messages_as_given_and_ends_input() {
    let transport = MockTransport::new();
    let options = ClaudeAgentOptions::builder().verbatim_prompts(true).build();
    let mut client = make_client(options, &transport);
    client
        .connect_with_stream(futures::stream::iter([serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "streamed"}
        })]))
        .await
        .unwrap();
    // Nothing needs the control channel: stdin closes once the stream ends.
    transport
        .wait_until("input closed", |s| s.input_closed)
        .await;
    let users = transport.lock().writes_of("user");
    assert_eq!(
        users[0],
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "streamed"},
            "client_composed": true
        })
    );
}

#[tokio::test]
async fn connect_with_stream_keeps_stdin_open_until_the_run_ends() {
    let transport = MockTransport::new();
    let options = ClaudeAgentOptions::builder()
        .can_use_tool(|_, _, _| async { Ok(PermissionResult::allow()) })
        .build();
    let mut client = make_client(options, &transport);
    client
        .connect_with_stream(futures::stream::iter([serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "streamed"}
        })]))
        .await
        .unwrap();
    transport
        .wait_until("prompt written", |s| !s.writes_of("user").is_empty())
        .await;
    transport.push(can_use_tool_request("perm", "Bash"));
    transport
        .wait_until("permission answered", |s| s.response_for("perm").is_some())
        .await;
    assert!(!transport.lock().input_closed);
    transport.push(result_frame());
    transport
        .wait_until("input closed", |s| s.input_closed)
        .await;
    let messages = client.collect_response().await.unwrap();
    assert!(matches!(messages.last(), Some(Message::ResultMsg { .. })));
}

#[tokio::test]
async fn receive_response_stream_does_not_borrow_the_client() {
    let transport = MockTransport::new();
    transport.lock().auto_control_responses = true;
    let mut client = make_client(ClaudeAgentOptions::default(), &transport);
    client.connect().await.unwrap();
    let stream = client.receive_response();
    // Control methods stay usable while a turn is streaming.
    client.interrupt().await.unwrap();
    transport.push(result_frame());
    let messages: Vec<_> = stream.try_collect().await.unwrap();
    assert_eq!(messages.len(), 1);
}
