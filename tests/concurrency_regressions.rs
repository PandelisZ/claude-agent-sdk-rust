//! Regression tests for the runtime's concurrency guarantees: a slow or
//! panicking `SessionStore` never stalls or kills frame routing, a cancelled
//! `connect()` leaves the client cleanly disconnected, a panicking prompt
//! stream still closes stdin, and frames that precede a corrupt stdout line
//! are delivered before the decode error.

mod support;

use async_trait::async_trait;
use claude_code_sdk_rust::session_store::{SessionKey, SessionStore, SessionStoreEntry};
use claude_code_sdk_rust::{
    query_messages_with_transport, ClaudeAgentClient, ClaudeAgentOptions, Message, Result,
    SessionStoreFlushMode,
};
use futures::StreamExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::{assistant, result_frame, MockTransport};

/// Sleeps on every append, then records the entry uuids.
#[derive(Clone)]
struct SlowStore {
    delay: Duration,
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl SessionStore for SlowStore {
    async fn append(&self, _key: SessionKey, entries: Vec<SessionStoreEntry>) -> Result<()> {
        tokio::time::sleep(self.delay).await;
        let mut seen = self.seen.lock().unwrap();
        for entry in entries {
            seen.push(entry["uuid"].as_str().unwrap_or_default().to_string());
        }
        Ok(())
    }

    async fn load(&self, _key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        Ok(None)
    }
}

/// Panics on every append.
#[derive(Clone)]
struct PanickingStore {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl SessionStore for PanickingStore {
    async fn append(&self, _key: SessionKey, _entries: Vec<SessionStoreEntry>) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("store exploded");
    }

    async fn load(&self, _key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        Ok(None)
    }
}

fn temp_config_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("claude-rust-concurrency-{}", uuid::Uuid::new_v4()))
}

fn mirror_frame(config_dir: &std::path::Path, uuid: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "transcript_mirror",
        "filePath": config_dir.join("projects/proj/session-1.jsonl"),
        "entries": [{"type": "user", "uuid": uuid}]
    })
}

#[tokio::test]
async fn slow_eager_store_does_not_stall_control_responses() {
    let config_dir = temp_config_dir();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = SlowStore {
        delay: Duration::from_secs(2),
        seen: seen.clone(),
    };
    let transport = MockTransport::new();
    transport.lock().auto_control_responses = true;
    transport.push(mirror_frame(&config_dir, "entry-1"));
    let options = ClaudeAgentOptions::builder()
        .env_var(
            "CLAUDE_CONFIG_DIR",
            config_dir.to_string_lossy().to_string(),
        )
        .session_store(store)
        .session_store_flush(SessionStoreFlushMode::Eager)
        .build();
    let mut client = ClaudeAgentClient::with_transport(options, transport.boxed()).unwrap();
    client.connect().await.unwrap();
    transport
        .wait_until("mirror frame read", |s| {
            s.position("read:transcript_mirror").is_some()
        })
        .await;

    // The eager flush is waiting on the store; the router must still route
    // the interrupt's control response.
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(1), client.interrupt())
        .await
        .expect("interrupt stalled behind the mirror flush")
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(seen.lock().unwrap().is_empty(), "append still in flight");

    // The final flush waits for the in-flight drain.
    client.disconnect().await.unwrap();
    assert_eq!(*seen.lock().unwrap(), ["entry-1"]);
}

#[tokio::test]
async fn slow_store_result_flush_waits_for_the_in_flight_drain() {
    let config_dir = temp_config_dir();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = SlowStore {
        delay: Duration::from_millis(300),
        seen: seen.clone(),
    };
    let transport = MockTransport::new();
    transport.push(mirror_frame(&config_dir, "entry-1"));
    transport.push(mirror_frame(&config_dir, "entry-2"));
    transport.push(assistant("hello"));
    transport.push(result_frame());
    let options = ClaudeAgentOptions::builder()
        .env_var(
            "CLAUDE_CONFIG_DIR",
            config_dir.to_string_lossy().to_string(),
        )
        .session_store(store)
        .session_store_flush(SessionStoreFlushMode::Eager)
        .build();
    let mut client = ClaudeAgentClient::with_transport(options, transport.boxed()).unwrap();
    client.connect().await.unwrap();
    client.query("hi").await.unwrap();
    let mut messages = client.receive_response();
    let mut saw_result = false;
    while let Some(message) = messages.next().await {
        if matches!(message.unwrap(), Message::ResultMsg { .. }) {
            // Both background drains landed, in order, before the result.
            assert_eq!(*seen.lock().unwrap(), ["entry-1", "entry-2"]);
            saw_result = true;
        }
    }
    assert!(saw_result);
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn panicking_store_reports_a_mirror_error_and_keeps_the_session_alive() {
    let config_dir = temp_config_dir();
    let calls = Arc::new(AtomicUsize::new(0));
    let transport = MockTransport::new();
    transport.lock().auto_control_responses = true;
    transport.push(mirror_frame(&config_dir, "entry-1"));
    transport.push(result_frame());
    let options = ClaudeAgentOptions::builder()
        .env_var(
            "CLAUDE_CONFIG_DIR",
            config_dir.to_string_lossy().to_string(),
        )
        .session_store(PanickingStore {
            calls: calls.clone(),
        })
        .build();
    let mut client = ClaudeAgentClient::with_transport(options, transport.boxed()).unwrap();
    client.connect().await.unwrap();
    client.query("hi").await.unwrap();

    let messages = tokio::time::timeout(Duration::from_secs(10), client.collect_response())
        .await
        .expect("the response must end")
        .unwrap();
    assert_eq!(messages.len(), 2, "{messages:?}");
    match &messages[0] {
        Message::MirrorErrorMsg(error) => {
            assert!(error.error.contains("store exploded"), "{}", error.error)
        }
        other => panic!("expected a mirror error first, got {other:?}"),
    }
    assert!(matches!(messages[1], Message::ResultMsg { .. }));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "a panic is a failed attempt"
    );

    // The router is still alive: control requests are answered at once.
    tokio::time::timeout(Duration::from_secs(1), client.interrupt())
        .await
        .expect("interrupt must not hang")
        .unwrap();
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn panicking_store_in_one_shot_query_still_delivers_the_result() {
    let config_dir = temp_config_dir();
    let transport = MockTransport::new();
    transport.push(mirror_frame(&config_dir, "entry-1"));
    transport.push(result_frame());
    transport.lock().eof_when_drained = true;
    let options = ClaudeAgentOptions::builder()
        .env_var(
            "CLAUDE_CONFIG_DIR",
            config_dir.to_string_lossy().to_string(),
        )
        .session_store(PanickingStore {
            calls: Arc::new(AtomicUsize::new(0)),
        })
        .build();
    let messages = tokio::time::timeout(
        Duration::from_secs(10),
        query_messages_with_transport("hi", Some(options), transport.boxed()),
    )
    .await
    .expect("the query must end")
    .unwrap();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(matches!(messages[0], Message::MirrorErrorMsg(_)));
    assert!(matches!(messages[1], Message::ResultMsg { .. }));
}

#[tokio::test]
async fn cancelled_connect_leaves_the_client_disconnected_not_wedged() {
    let transport = MockTransport::new();
    // The CLI never answers initialize.
    transport.lock().init_response = None;
    let mut client =
        ClaudeAgentClient::with_transport(ClaudeAgentOptions::default(), transport.boxed())
            .unwrap();

    let outcome = tokio::time::timeout(Duration::from_millis(200), client.connect()).await;
    assert!(outcome.is_err(), "connect should still be waiting");

    // The half-started session is torn down, not left behind.
    transport.wait_until("transport closed", |s| s.closed).await;
    assert!(client.get_server_info().is_err());
    // A later connect must not report success over a dead session.
    let reconnect = client.connect().await;
    assert!(
        reconnect.is_err(),
        "connect() returned Ok over a dead session"
    );
    let error = client.query("hi").await.unwrap_err();
    assert!(error.to_string().contains("Not connected"), "{error}");
}

#[tokio::test]
async fn panicking_prompt_stream_still_closes_stdin() {
    let transport = MockTransport::new();
    let mut client =
        ClaudeAgentClient::with_transport(ClaudeAgentOptions::default(), transport.boxed())
            .unwrap();
    let prompts = futures::stream::iter(0..2).map(|index| {
        if index == 1 {
            panic!("prompt source failed");
        }
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "first"},
            "parent_tool_use_id": null,
            "session_id": "default"
        })
    });
    client.connect_with_stream(prompts).await.unwrap();
    transport
        .wait_until("stdin closed", |s| s.input_closed)
        .await;
    assert_eq!(transport.lock().writes_of("user").len(), 1);
    client.disconnect().await.unwrap();
}

#[cfg(unix)]
mod subprocess {
    use claude_code_sdk_rust::{stream_query, ClaudeAgentOptions, ClaudeSDKError, Message};
    use futures::StreamExt;
    use std::path::PathBuf;
    use std::time::Duration;

    /// Write an executable `/bin/sh` script. The file handle is closed
    /// before the script can be executed (avoids ETXTBSY).
    fn fake_cli(body: &str) -> (PathBuf, PathBuf) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "claude-rust-concurrency-cli-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude");
        {
            let mut file = std::fs::File::create(&script).unwrap();
            write!(file, "#!/bin/sh\n{body}").unwrap();
            file.sync_all().unwrap();
        }
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        (dir, script)
    }

    #[tokio::test]
    async fn result_before_a_corrupt_line_in_one_write_is_delivered() {
        // Answer initialize, read the prompt, then write the result and a
        // corrupt line with a single printf (one write, one read chunk).
        let (dir, script) = fake_cli(
            r#"read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"commands":[]}}}\n' "$id"
read -r prompt
printf '%s\n%s\n' '{"type":"result","subtype":"success","duration_ms":1,"duration_api_ms":1,"is_error":false,"num_turns":1,"session_id":"s"}' '{"type": oops'
while read -r line; do :; done
"#,
        );
        let options = ClaudeAgentOptions::builder()
            .cli_path(script.to_string_lossy().to_string())
            .build();
        let items: Vec<_> = tokio::time::timeout(
            Duration::from_secs(20),
            stream_query("hi", Some(options)).collect::<Vec<_>>(),
        )
        .await
        .expect("the query must end");
        assert_eq!(items.len(), 2, "{items:?}");
        assert!(
            matches!(items[0], Ok(Message::ResultMsg { .. })),
            "the result was lost: {items:?}"
        );
        assert!(
            matches!(items[1], Err(ClaudeSDKError::CLIJSONDecode(_))),
            "{items:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
