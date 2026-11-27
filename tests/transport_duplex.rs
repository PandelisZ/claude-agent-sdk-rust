//! Regression tests: reading the CLI's stdout and writing its stdin progress
//! independently. Each fake CLI floods stdout (far past a pipe buffer)
//! without reading stdin while the SDK writes more than a stdin pipe buffer.
//! If a pending stdin write kept the SDK from draining stdout, both sides
//! would block forever; every test fails on a timeout instead.
#![cfg(unix)]

use claude_code_sdk_rust::{
    query_messages, query_stream_messages, ClaudeAgentClient, ClaudeAgentOptions, Message,
    PermissionResult,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Stdin payload per test: well past the 64 KiB pipe buffer and over 1 MiB.
const PROMPT_BYTES: usize = 2 * 1024 * 1024;
/// stdout lines (about 1 KB each) the CLI writes before it reads stdin.
const FLOOD_LINES: usize = 2048;
const DEADLINE: Duration = Duration::from_secs(30);

/// Write an executable `/bin/sh` script. The file handle is closed before
/// the script can be executed (avoids ETXTBSY).
fn fake_cli(body: &str) -> (PathBuf, PathBuf) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!(
        "claude-rust-transport-duplex-{}",
        uuid::Uuid::new_v4()
    ));
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

/// Shell helpers. `flood N` writes N assistant messages of about 1 KB each
/// without touching stdin.
const PRELUDE: &str = r#"
d="$(dirname "$0")"
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
pad=x
while [ ${#pad} -lt 1000 ]; do pad="$pad$pad"; done
flood() {
  i=0
  while [ $i -lt "$1" ]; do assistant "$pad"; i=$((i+1)); done
}
drain_stdin() { while read -r line; do :; done; }
"#;

fn options_for(script: &Path) -> claude_code_sdk_rust::ClaudeAgentOptionsBuilder {
    ClaudeAgentOptions::builder().cli_path(script.to_string_lossy().to_string())
}

fn read_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn count_assistants(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|m| matches!(m, Message::AssistantMsg { .. }))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_large_prompt_while_the_cli_floods_stdout() {
    let (dir, script) = fake_cli(&format!(
        r#"answer_init
flood {FLOOD_LINES}
head -n 1 > "$d/prompt.json"
result
drain_stdin
"#
    ));
    let mut client = ClaudeAgentClient::new(options_for(&script).build()).unwrap();
    let outcome = tokio::time::timeout(DEADLINE, async {
        client.connect().await.unwrap();
        client.query("y".repeat(PROMPT_BYTES)).await.unwrap();
        let messages = client.collect_response().await.unwrap();
        client.disconnect().await.unwrap();
        messages
    })
    .await;
    let messages = outcome.expect("stdout must drain while the prompt is written");
    assert_eq!(messages.len(), FLOOD_LINES + 1);
    assert_eq!(count_assistants(&messages), FLOOD_LINES);
    assert!(matches!(messages.last(), Some(Message::ResultMsg { .. })));

    let prompt = read_lines(&dir.join("prompt.json"));
    assert_eq!(prompt.len(), 1);
    assert_eq!(
        prompt[0]["message"]["content"].as_str().map(str::len),
        Some(PROMPT_BYTES)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamed_prompts_while_the_cli_floods_stdout_arrive_in_order() {
    const PROMPTS: usize = 4;
    let (dir, script) = fake_cli(&format!(
        r#"answer_init
flood {FLOOD_LINES}
head -n {PROMPTS} > "$d/prompts.jsonl"
result
"#
    ));
    let size = PROMPT_BYTES / PROMPTS;
    let prompts: Vec<Value> = (0..PROMPTS)
        .map(|i| {
            json!({
                "type": "user",
                "session_id": "",
                "message": {"role": "user", "content": format!("{i}{}", "z".repeat(size))},
                "parent_tool_use_id": null,
            })
        })
        .collect();
    let messages = tokio::time::timeout(
        DEADLINE,
        query_stream_messages(
            futures::stream::iter(prompts),
            Some(options_for(&script).build()),
        ),
    )
    .await
    .expect("stdout must drain while the prompts are written")
    .expect("query succeeds");
    assert_eq!(messages.len(), FLOOD_LINES + 1);
    assert_eq!(count_assistants(&messages), FLOOD_LINES);

    let received = read_lines(&dir.join("prompts.jsonl"));
    assert_eq!(received.len(), PROMPTS);
    for (i, prompt) in received.iter().enumerate() {
        let content = prompt["message"]["content"].as_str().unwrap();
        assert!(
            content.starts_with(&i.to_string()),
            "prompt {i} out of order"
        );
        assert_eq!(content.len(), size + 1);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A control request arrives mid-flood while the large prompt is still
/// queued: its answer is written behind the prompt, and neither blocks the
/// reader.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_response_behind_a_large_prompt_while_the_cli_floods_stdout() {
    let half = FLOOD_LINES / 2;
    let (dir, script) = fake_cli(&format!(
        r#"answer_init
flood {half}
printf '{{"type":"control_request","request_id":"perm_1","request":{{"subtype":"can_use_tool","tool_name":"Bash","input":{{"command":"ls"}}}}}}\n'
flood {half}
head -n 2 > "$d/lines.jsonl"
assistant done
result
drain_stdin
"#
    ));
    let options = options_for(&script)
        .can_use_tool(|_, _, _| async move { Ok(PermissionResult::allow()) })
        .build();
    let messages = tokio::time::timeout(
        DEADLINE,
        query_messages("y".repeat(PROMPT_BYTES), Some(options)),
    )
    .await
    .expect("stdout must drain while stdin writes are pending")
    .expect("query succeeds");
    assert_eq!(messages.len(), FLOOD_LINES + 2);
    assert_eq!(count_assistants(&messages), FLOOD_LINES + 1);

    let lines = read_lines(&dir.join("lines.jsonl"));
    assert_eq!(lines.len(), 2);
    let prompt = lines
        .iter()
        .find(|line| line["type"] == "user")
        .expect("prompt");
    assert_eq!(
        prompt["message"]["content"].as_str().map(str::len),
        Some(PROMPT_BYTES)
    );
    let answer = lines
        .iter()
        .find(|line| line["type"] == "control_response")
        .expect("permission answer");
    assert_eq!(answer["response"]["request_id"], "perm_1");
    assert_eq!(answer["response"]["response"]["behavior"], "allow");
    let _ = std::fs::remove_dir_all(&dir);
}
