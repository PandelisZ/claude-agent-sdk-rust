mod support;

use claude_code_sdk_rust::{
    create_sdk_mcp_server, query_messages_with_transport, query_stream_messages_with_transport,
    stream_query_with_transport, tool, ClaudeAgentOptions, ClaudeSDKError, HookCallback,
    HookMatcher, Message, PermissionResult, QueryPrompt,
};
use futures::StreamExt;
use std::time::Duration;
use support::{
    assistant, error_result_frame, result_frame, session_state, MockTransport, ReadItem,
};

fn hook_options() -> ClaudeAgentOptions {
    ClaudeAgentOptions::builder()
        .hook(
            "PreToolUse",
            HookMatcher::new(HookCallback::new(|_, _, _| async {
                Ok(serde_json::json!({}))
            })),
        )
        .build()
}

#[tokio::test]
async fn query_messages_with_transport_answers_post_prompt_sdk_mcp_control_requests() {
    let transport = MockTransport::new();
    transport.push(serde_json::json!({
        "type": "control_request",
        "request_id": "mcp_1",
        "request": {
            "subtype": "mcp_message",
            "server_name": "people",
            "message": {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "greet", "arguments": {"name": "Ada"}}
            }
        }
    }));
    transport.push(result_frame());
    transport.lock().eof_when_drained = true;

    let server = create_sdk_mcp_server(
        "people",
        vec![tool(
            "greet",
            "Greet a user",
            serde_json::json!({"type": "object"}),
            |input| {
                let name = input
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("there");
                Ok(vec![claude_code_sdk_rust::MCPContent::Text {
                    text: format!("Hello, {name}!"),
                }])
            },
        )],
    );
    let options = ClaudeAgentOptions::builder()
        .sdk_mcp_server("people", server)
        .build();

    let messages = query_messages_with_transport("hi", Some(options), transport.boxed())
        .await
        .expect("query");

    assert!(messages
        .iter()
        .any(|message| matches!(message, Message::ResultMsg { .. })));
    let state = transport.lock();
    let mcp_response = state.response_for("mcp_1").expect("mcp response");
    assert_eq!(
        mcp_response["response"]["response"]["mcp_response"]["result"]["content"][0]["text"],
        "Hello, Ada!"
    );
    assert!(state.closed);
}

#[tokio::test]
async fn string_query_writes_python_user_frame_and_closes_stdin_at_once() {
    let transport = MockTransport::new();
    // Like the CLI, only answer once stdin is closed: a query that waited
    // for the result before closing stdin would hang here.
    let options = ClaudeAgentOptions::builder().verbatim_prompts(true).build();
    let query = query_messages_with_transport("hi", Some(options), transport.boxed());
    let script = async {
        transport
            .wait_until("stdin closed", |s| s.input_closed)
            .await;
        transport.push(result_frame());
        transport.push_item(ReadItem::Eof);
    };
    let (messages, ()) = tokio::join!(query, script);
    assert_eq!(messages.unwrap().len(), 1);
    let users = transport.lock().writes_of("user");
    assert_eq!(
        serde_json::to_string(&users[0]).unwrap(),
        r#"{"type":"user","session_id":"","message":{"role":"user","content":"hi"},"parent_tool_use_id":null,"client_composed":true}"#
    );
}

#[tokio::test]
async fn query_reads_every_result_until_stdout_ends() {
    let transport = MockTransport::new();
    transport.push(assistant("first turn"));
    transport.push(result_frame());
    transport.push(assistant("woken by a background task"));
    transport.push(result_frame());
    transport.lock().eof_when_drained = true;

    let messages = query_messages_with_transport("hi", None, transport.boxed())
        .await
        .unwrap();
    assert_eq!(messages.len(), 4);
    assert!(matches!(messages[3], Message::ResultMsg { .. }));
}

#[tokio::test]
async fn string_query_allows_can_use_tool_and_answers_it() {
    let transport = MockTransport::new();
    let options = ClaudeAgentOptions::builder()
        .can_use_tool(|tool_name, _, _| async move {
            assert_eq!(tool_name, "Write");
            Ok(PermissionResult::deny("nope"))
        })
        .build();
    let query = query_messages_with_transport("hi", Some(options), transport.boxed());
    let script = async {
        transport
            .wait_until("prompt", |s| !s.writes_of("user").is_empty())
            .await;
        transport.push(serde_json::json!({
            "type": "control_request",
            "request_id": "perm_1",
            "request": {"subtype": "can_use_tool", "tool_name": "Write", "input": {}}
        }));
        transport
            .wait_until("permission answered", |s| {
                s.response_for("perm_1").is_some()
            })
            .await;
        // stdin must still be open for the answer to have been written.
        assert!(!transport.lock().input_closed);
        transport.push(result_frame());
        transport
            .wait_until("stdin closed after the result", |s| s.input_closed)
            .await;
        transport.push_item(ReadItem::Eof);
    };
    let (messages, ()) = tokio::join!(query, script);
    assert_eq!(messages.unwrap().len(), 1);
    let state = transport.lock();
    assert_eq!(
        state.response_for("perm_1").unwrap()["response"]["response"],
        serde_json::json!({"behavior": "deny", "message": "nope"})
    );
    assert!(state.position("read:result").unwrap() < state.position("close_input").unwrap());
}

#[tokio::test]
async fn bidirectional_query_waits_for_idle_session_state() {
    let transport = MockTransport::new();
    let query = query_messages_with_transport("hi", Some(hook_options()), transport.boxed());
    let script = async {
        transport
            .wait_until("prompt", |s| !s.writes_of("user").is_empty())
            .await;
        transport.push(session_state("running", true));
        transport.push(result_frame());
        transport
            .wait_until("result read", |s| s.position("read:result").is_some())
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The CLI still reports "running": a follow-up turn may need stdin.
        assert!(!transport.lock().input_closed);
        transport.push(session_state("idle", true));
        transport
            .wait_until("stdin closed at idle", |s| s.input_closed)
            .await;
        transport.push_item(ReadItem::Eof);
    };
    let (messages, ()) = tokio::join!(query, script);
    let messages = messages.unwrap();
    // The sdk_host_only frames are not yielded.
    assert_eq!(messages.len(), 1, "{messages:?}");
}

#[tokio::test]
async fn tracked_background_agent_keeps_stdin_open_past_the_result() {
    let transport = MockTransport::new();
    let query = query_messages_with_transport("hi", Some(hook_options()), transport.boxed());
    let script = async {
        transport
            .wait_until("prompt", |s| !s.writes_of("user").is_empty())
            .await;
        transport.push(serde_json::json!({
            "type": "system", "subtype": "task_started", "task_id": "t1",
            "task_type": "local_agent", "description": "bg", "uuid": "u1", "session_id": "s"
        }));
        transport.push(result_frame());
        transport
            .wait_until("result read", |s| s.position("read:result").is_some())
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!transport.lock().input_closed);
        transport.push(serde_json::json!({
            "type": "system", "subtype": "task_notification", "task_id": "t1",
            "status": "completed", "output_file": "", "summary": "done",
            "uuid": "u2", "session_id": "s"
        }));
        transport.push(assistant("follow-up"));
        transport.push(result_frame());
        transport
            .wait_until("stdin closed after follow-up", |s| s.input_closed)
            .await;
        transport.push_item(ReadItem::Eof);
    };
    let (messages, ()) = tokio::join!(query, script);
    let messages = messages.unwrap();
    assert!(matches!(messages.last(), Some(Message::ResultMsg { .. })));
}

#[tokio::test]
async fn run_end_ceiling_closes_stdin_when_idle_never_comes() {
    let transport = MockTransport::new();
    let mut options = hook_options();
    options.env.insert(
        "CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS".to_string(),
        "50".to_string(),
    );
    let query = query_messages_with_transport("hi", Some(options), transport.boxed());
    let script = async {
        transport
            .wait_until("prompt", |s| !s.writes_of("user").is_empty())
            .await;
        transport.push(session_state("running", true));
        transport.push(result_frame());
        transport
            .wait_until("stdin closed at the ceiling", |s| s.input_closed)
            .await;
        transport.push_item(ReadItem::Eof);
    };
    let (messages, ()) = tokio::join!(query, script);
    messages.unwrap();
}

#[tokio::test]
async fn query_stream_messages_writes_frames_as_given_and_waits_for_the_run() {
    let transport = MockTransport::new();
    let stream = futures::stream::iter(vec![
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "first"}
        }),
        serde_json::json!({
            "type": "user",
            "session_id": "custom",
            "message": {"role": "user", "content": "second"}
        }),
    ]);
    let query =
        query_stream_messages_with_transport(stream, Some(hook_options()), transport.boxed());
    let script = async {
        transport
            .wait_until("both prompts", |s| s.writes_of("user").len() == 2)
            .await;
        // Hooks configured: stdin stays open until the run ends.
        assert!(!transport.lock().input_closed);
        transport.push(result_frame());
        transport
            .wait_until("stdin closed", |s| s.input_closed)
            .await;
        transport.push_item(ReadItem::Eof);
    };
    let (messages, ()) = tokio::join!(query, script);
    assert_eq!(messages.unwrap().len(), 1);
    let state = transport.lock();
    let users = state.writes_of("user");
    // No session_id is injected (Python query(prompt=AsyncIterable)).
    assert!(users[0].get("session_id").is_none());
    assert_eq!(users[1]["session_id"], "custom");
    assert!(state.closed);
}

#[tokio::test]
async fn empty_prompt_stream_closes_stdin_at_once() {
    let transport = MockTransport::new();
    let query = query_stream_messages_with_transport(
        futures::stream::empty(),
        Some(hook_options()),
        transport.boxed(),
    );
    let script = async {
        transport
            .wait_until("stdin closed", |s| s.input_closed)
            .await;
        transport.push_item(ReadItem::Eof);
    };
    let (messages, ()) = tokio::join!(query, script);
    assert!(messages.unwrap().is_empty());
}

#[tokio::test]
async fn query_raises_result_error_after_an_error_result() {
    let transport = MockTransport::new();
    transport.push(error_result_frame());
    transport.push_item(ReadItem::ProcessExit(1));
    let err = query_messages_with_transport("hi", None, transport.boxed())
        .await
        .unwrap_err();
    match err {
        ClaudeSDKError::Result(error) => {
            assert_eq!(
                error.to_string(),
                "Claude Code returned an error result: Reached maximum number of turns (3) (exit code: 1)"
            );
            assert_eq!(error.data["subtype"], "error_max_turns");
        }
        other => panic!("expected ResultError, got {other:?}"),
    }
    assert!(transport.lock().closed);
}

#[tokio::test]
async fn plain_process_error_is_not_replaced() {
    let transport = MockTransport::new();
    transport.push(error_result_frame());
    transport.push(assistant("the conversation moved on"));
    transport.push_item(ReadItem::ProcessExit(2));
    let err = query_messages_with_transport("hi", None, transport.boxed())
        .await
        .unwrap_err();
    assert!(matches!(err, ClaudeSDKError::Process(_)), "{err:?}");
}

#[tokio::test]
async fn initialize_failure_closes_the_transport() {
    let transport = MockTransport::new();
    transport.lock().init_response = None;
    transport.push_item(ReadItem::Eof);
    let err = query_messages_with_transport("hi", None, transport.boxed())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("initialize"), "{err}");
    assert!(transport.lock().closed);
}

/// A live transport whose prompt writes fail (control frames still work).
struct FailingPromptWrites(MockTransport);

#[async_trait::async_trait]
impl claude_code_sdk_rust::Transport for FailingPromptWrites {
    async fn connect(&mut self) -> claude_code_sdk_rust::Result<()> {
        self.0.connect().await
    }
    async fn write(&mut self, data: &[u8]) -> claude_code_sdk_rust::Result<()> {
        if data.starts_with(br#"{"type":"user""#) {
            return Err(claude_code_sdk_rust::CLIConnectionError::new("stdin broke").into());
        }
        self.0.write(data).await
    }
    async fn close_input(&mut self) -> claude_code_sdk_rust::Result<()> {
        self.0.close_input().await
    }
    async fn read(&mut self) -> claude_code_sdk_rust::Result<Option<Vec<u8>>> {
        self.0.read().await
    }
    async fn close(&mut self) -> claude_code_sdk_rust::Result<()> {
        self.0.close().await
    }
}

#[tokio::test]
async fn failed_prompt_write_fails_a_bidirectional_query_instead_of_hanging() {
    let transport = MockTransport::new();
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        query_messages_with_transport(
            "hi",
            Some(hook_options()),
            Box::new(FailingPromptWrites(transport.clone())),
        ),
    )
    .await
    .expect("a failed prompt write must not wait for a run that cannot start")
    .unwrap_err();
    assert_eq!(err.to_string(), "stdin broke");
    assert!(transport.lock().closed);
}

#[tokio::test]
async fn connect_failure_is_returned() {
    let transport = MockTransport::new();
    transport.lock().connect_error = Some("no CLI".to_string());
    let err = query_messages_with_transport("hi", None, transport.boxed())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "no CLI");
}

#[tokio::test]
async fn stream_query_yields_messages_as_they_arrive() {
    let transport = MockTransport::new();
    let mut messages = stream_query_with_transport("hi", None, transport.boxed());
    transport.push(assistant("one"));
    let first = tokio::time::timeout(Duration::from_secs(5), messages.next())
        .await
        .expect("streamed before the run ends")
        .unwrap()
        .unwrap();
    assert!(matches!(first, Message::AssistantMsg { .. }));
    transport.push(result_frame());
    transport.push_item(ReadItem::Eof);
    let rest: Vec<_> = messages.collect().await;
    assert_eq!(rest.len(), 1);
    transport.wait_until("closed", |s| s.closed).await;
}

#[tokio::test]
async fn stream_query_accepts_a_message_stream_prompt() {
    let transport = MockTransport::new();
    let prompt = QueryPrompt::messages(futures::stream::iter([serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": "streamed"}
    })]));
    // The query starts on the first poll.
    let messages = stream_query_with_transport(prompt, None, transport.boxed());
    let script = async {
        transport
            .wait_until("stdin closed", |s| s.input_closed)
            .await;
        transport.push(result_frame());
        transport.push_item(ReadItem::Eof);
    };
    let (items, ()) = tokio::join!(messages.collect::<Vec<_>>(), script);
    assert_eq!(items.len(), 1);
    assert_eq!(
        transport.lock().writes_of("user")[0]["message"]["content"],
        "streamed"
    );
}

#[tokio::test]
async fn dropping_stream_query_closes_the_session() {
    let transport = MockTransport::new();
    let mut messages = stream_query_with_transport("hi", None, transport.boxed());
    transport.push(assistant("one"));
    messages.next().await.unwrap().unwrap();
    drop(messages);
    transport.wait_until("closed", |s| s.closed).await;
}

#[tokio::test]
async fn mirror_errors_reach_the_query_consumer() {
    // A store whose append always fails.
    #[derive(Clone)]
    struct FailingStore;
    #[async_trait::async_trait]
    impl claude_code_sdk_rust::SessionStore for FailingStore {
        async fn append(
            &self,
            _key: claude_code_sdk_rust::SessionKey,
            _entries: Vec<claude_code_sdk_rust::SessionStoreEntry>,
        ) -> claude_code_sdk_rust::Result<()> {
            Err(ClaudeSDKError::Other("disk full".to_string()))
        }
        async fn load(
            &self,
            _key: claude_code_sdk_rust::SessionKey,
        ) -> claude_code_sdk_rust::Result<Option<Vec<claude_code_sdk_rust::SessionStoreEntry>>>
        {
            Ok(None)
        }
    }
    let temp = std::env::temp_dir().join(format!("claude-rust-mirror-{}", uuid::Uuid::new_v4()));
    let transport = MockTransport::new();
    transport.push(serde_json::json!({
        "type": "transcript_mirror",
        "filePath": temp.join("projects/proj/s.jsonl"),
        "entries": [{"type": "user", "uuid": "e1"}]
    }));
    transport.push(result_frame());
    transport.lock().eof_when_drained = true;
    let options = ClaudeAgentOptions::builder()
        .env_var("CLAUDE_CONFIG_DIR", temp.to_string_lossy().to_string())
        .session_store(FailingStore)
        .build();
    let messages = query_messages_with_transport("hi", Some(options), transport.boxed())
        .await
        .unwrap();
    assert_eq!(messages.len(), 2, "{messages:?}");
    match &messages[0] {
        Message::MirrorErrorMsg(error) => assert_eq!(error.error, "disk full"),
        other => panic!("expected mirror error first, got {other:?}"),
    }
    assert!(matches!(messages[1], Message::ResultMsg { .. }));
}

#[test]
fn streaming_query_and_transport_types_are_exported_at_the_root() {
    use claude_code_sdk_rust::{
        stream_query, CanUseToolShadowedWarning, MessageStream, SubprocessCLITransport, Transport,
        TransportOptions,
    };
    fn is_transport<T: Transport>() {}
    is_transport::<SubprocessCLITransport>();
    let _ = TransportOptions::from(&ClaudeAgentOptions::default());
    let _: fn(&str, Option<ClaudeAgentOptions>) -> MessageStream = |p, o| stream_query(p, o);
    let warning = CanUseToolShadowedWarning {
        message: "m".to_string(),
    };
    assert_eq!(warning.to_string(), "m");
    assert!(MockTransport::new().is_ready());
}
