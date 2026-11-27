//! End-to-end control-protocol tests against fake `claude` scripts run
//! through the real subprocess transport (`cli_path`).
#![cfg(unix)]

use claude_code_sdk_rust::{
    query_messages, ClaudeAgentClient, ClaudeAgentOptions, ClaudeSDKError, Message,
    PermissionResult,
};
use futures::StreamExt;
use std::path::PathBuf;
use std::time::Duration;

/// Write an executable `/bin/sh` script. The file handle is closed before
/// the script can be executed (avoids ETXTBSY).
fn fake_cli(body: &str) -> (PathBuf, PathBuf) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let dir =
        std::env::temp_dir().join(format!("claude-rust-runtime-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("claude");
    {
        let mut file = std::fs::File::create(&script).unwrap();
        write!(file, "#!/bin/sh\n{PRELUDE}{body}").unwrap();
        file.sync_all().unwrap();
    }
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    (dir, script)
}

/// Shell helpers: `answer_init` reads the initialize request and answers it.
const PRELUDE: &str = r#"
request_id() { printf '%s' "$1" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p'; }
answer_init() {
  read -r line
  id=$(request_id "$line")
  printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"commands":[]}}}\n' "$id"
}
assistant() {
  printf '{"type":"assistant","message":{"model":"m","content":[{"type":"text","text":"%s"}]},"session_id":"s"}\n' "$1"
}
result() {
  printf '{"type":"result","subtype":"success","duration_ms":1,"duration_api_ms":1,"is_error":false,"num_turns":1,"session_id":"s"}\n'
}
drain_stdin() { while read -r line; do :; done; }
"#;

fn options_for(script: &std::path::Path) -> claude_code_sdk_rust::ClaudeAgentOptionsBuilder {
    ClaudeAgentOptions::builder().cli_path(script.to_string_lossy().to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn string_prompt_can_use_tool_is_answered_before_stdin_closes() {
    let (dir, script) = fake_cli(
        r#"answer_init
read -r prompt
printf '%s\n' "$prompt" > "$(dirname "$0")/prompt.json"
printf '{"type":"control_request","request_id":"perm_1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}\n'
if ! read -r answer; then echo "stdin closed before the permission answer" >&2; exit 7; fi
case "$answer" in *'"behavior":"allow"'*) assistant allowed ;; *) assistant other ;; esac
result
drain_stdin
"#,
    );
    let options = options_for(&script)
        .can_use_tool(|tool_name, input, _| async move {
            assert_eq!(tool_name, "Bash");
            assert_eq!(input["command"], "ls");
            Ok(PermissionResult::allow())
        })
        .build();
    let messages =
        tokio::time::timeout(Duration::from_secs(20), query_messages("hi", Some(options)))
            .await
            .expect("query finishes")
            .expect("query succeeds");
    assert_eq!(messages.len(), 2, "{messages:?}");
    match &messages[0] {
        Message::AssistantMsg { content, .. } => {
            assert!(matches!(
                &content.content[0],
                claude_code_sdk_rust::ContentBlock::Text { text } if text == "allowed"
            ));
        }
        other => panic!("unexpected {other:?}"),
    }
    let prompt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("prompt.json")).unwrap()).unwrap();
    assert_eq!(prompt["message"]["content"], "hi");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn error_result_then_exit_raises_result_error() {
    let (dir, script) = fake_cli(
        r#"answer_init
read -r prompt
printf '{"type":"result","subtype":"success","duration_ms":1,"duration_api_ms":1,"is_error":true,"num_turns":1,"session_id":"s","errors":[],"result":"API Error: 529 overloaded","api_error_status":529,"terminal_reason":"api_error"}\n'
exit 1
"#,
    );
    let err = tokio::time::timeout(
        Duration::from_secs(20),
        query_messages("hi", Some(options_for(&script).build())),
    )
    .await
    .expect("query finishes")
    .unwrap_err();
    match err {
        ClaudeSDKError::Result(error) => {
            assert_eq!(
                error.message,
                "Claude Code returned an error result: API Error: 529 overloaded"
            );
            assert_eq!(error.exit_code, Some(1));
            assert_eq!(error.api_error_status, Some(529));
            assert_eq!(error.terminal_reason.as_deref(), Some("api_error"));
        }
        other => panic!("expected ResultError, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_mid_turn_keeps_the_turns_messages() {
    let (dir, script) = fake_cli(
        r#"answer_init
read -r prompt
assistant partial
read -r request
id=$(request_id "$request")
assistant "after interrupt"
printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$id"
result
drain_stdin
"#,
    );
    let mut client = ClaudeAgentClient::new(options_for(&script).build()).unwrap();
    client.connect().await.unwrap();
    client.query("hi").await.unwrap();
    let mut stream = client.receive_response();
    let first = stream.next().await.unwrap().unwrap();
    assert!(matches!(first, Message::AssistantMsg { .. }));
    tokio::time::timeout(Duration::from_secs(20), client.interrupt())
        .await
        .expect("interrupt answered")
        .unwrap();
    let rest: Vec<_> = stream.collect().await;
    assert_eq!(rest.len(), 2, "{rest:?}");
    assert!(matches!(rest[1], Ok(Message::ResultMsg { .. })));
    client.disconnect().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dropping_a_connected_client_ends_the_cli() {
    let (dir, script) = fake_cli(
        r#"printf '%s' "$$" > "$(dirname "$0")/pid"
answer_init
drain_stdin
"#,
    );
    let mut client = ClaudeAgentClient::new(options_for(&script).build()).unwrap();
    client.connect().await.unwrap();
    let pid = std::fs::read_to_string(dir.join("pid")).unwrap();
    drop(client);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let alive = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if !alive {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "CLI {pid} still running"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = std::fs::remove_dir_all(&dir);
}
