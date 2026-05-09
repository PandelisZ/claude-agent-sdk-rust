use claude_agent_sdk::{query, ClaudeAgentOptions};

const CLAUDE_HAIKU_4_5_MODEL: &str = "claude-haiku-4-5-20251001";

fn has_real_claude_auth() -> bool {
    std::env::var_os("ANTHROPIC_API_KEY").is_some()
        || std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN").is_some()
}

#[tokio::test]
#[ignore = "requires Claude CLI authentication and may incur API usage"]
async fn real_claude_cli_query_smoke() {
    if !has_real_claude_auth() {
        eprintln!("skipping: ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN is required");
        return;
    }

    let options = ClaudeAgentOptions::builder().max_turns(1).build();
    let result = query("Reply with exactly: pong", Some(options))
        .await
        .expect("real Claude CLI query should succeed");

    assert!(
        result.content.to_ascii_lowercase().contains("pong"),
        "expected response to contain pong, got {:?}",
        result.content
    );
}

#[tokio::test]
#[ignore = "requires Claude CLI authentication and may incur API usage"]
async fn real_claude_cli_haiku_4_5_query_smoke() {
    if !has_real_claude_auth() {
        eprintln!("skipping: ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN is required");
        return;
    }

    let options = ClaudeAgentOptions::builder()
        .model(CLAUDE_HAIKU_4_5_MODEL)
        .max_turns(1)
        .build();
    let result = query("Reply with exactly: haiku-pong", Some(options))
        .await
        .expect("real Claude CLI Haiku 4.5 query should succeed");

    assert_eq!(
        result.content.trim(),
        "haiku-pong",
        "expected exact Haiku 4.5 response, got {:?}",
        result.content
    );
}
