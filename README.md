# claude-code-sdk-rust

[![crates.io](https://img.shields.io/crates/v/claude-code-sdk-rust.svg)](https://crates.io/crates/claude-code-sdk-rust)
[![docs.rs](https://docs.rs/claude-code-sdk-rust/badge.svg)](https://docs.rs/claude-code-sdk-rust)
[![license](https://img.shields.io/crates/l/claude-code-sdk-rust.svg)](./LICENSE)

An async Rust SDK for the Claude Code CLI.

It drives an authenticated local `claude` CLI process with typed options,
streamed message parsing, interactive sessions, control requests, hooks,
permission callbacks, in-process SDK MCP servers, and local/session-store
helpers. The API is modeled on the Python
[`claude-agent-sdk`](https://github.com/anthropics/claude-agent-sdk-python),
using Rust-native async streams, traits and typed data structures.

**Parity target:** Python `claude-agent-sdk` **v0.2.189**.

> This is a community SDK, not an official Anthropic package. The crate is
> published as `claude-code-sdk-rust` and imported as `claude_code_sdk_rust`.

## Requirements

- Rust (edition 2021) and a Tokio runtime
- Claude Code CLI installed and authenticated, on `PATH` or set with
  `ClaudeAgentOptions::builder().cli_path(...)`

## Installation

```toml
[dependencies]
claude-code-sdk-rust = "0.5"
tokio = { version = "1", features = ["full"] }
futures = "0.3"
```

Enable OpenTelemetry trace-context propagation with the `otel` feature:

```toml
[dependencies]
claude-code-sdk-rust = { version = "0.5", features = ["otel"] }
```

## Quick start

### One-shot query

`query` runs a prompt to completion and returns the summarized text.

```rust,no_run
use claude_code_sdk_rust::query;

#[tokio::main]
async fn main() -> claude_code_sdk_rust::Result<()> {
    let result = query("Explain ownership in Rust in two sentences.", None).await?;
    println!("{}", result.content);
    Ok(())
}
```

### Streaming one-shot query

`stream_query` is the Python `query()` async iterator: it yields each
`Message` as it arrives. The prompt is a string or a `QueryPrompt::messages`
stream of raw stream-json frames.

```rust,no_run
use claude_code_sdk_rust::{stream_query, ContentBlock, Message, QueryPrompt};
use futures::StreamExt;

#[tokio::main]
async fn main() -> claude_code_sdk_rust::Result<()> {
    let mut messages = stream_query("List the files you would inspect first.", None);
    while let Some(message) = messages.next().await {
        match message? {
            Message::AssistantMsg { content, .. } => {
                for block in content.content {
                    if let ContentBlock::Text { text } = block {
                        println!("{text}");
                    }
                }
            }
            Message::ResultMsg { total_cost_usd, .. } => println!("cost: {total_cost_usd:?}"),
            _ => {}
        }
    }

    // Streamed input: each value is written to the CLI as one JSON line.
    let frames = futures::stream::iter(vec![serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": "Hello"}
    })]);
    let mut messages = stream_query(QueryPrompt::messages(frames), None);
    while let Some(message) = messages.next().await {
        println!("{:?}", message?);
    }
    Ok(())
}
```

`query_messages` / `query_stream_messages` collect the same stream into a
`Vec<Message>`, and the `*_with_transport` variants accept any
`Box<dyn Transport>`.

## Interactive client

`ClaudeAgentClient` (alias `ClaudeSDKClient`) mirrors the Python client:
`connect()`, send prompts and control requests, read messages, then
`disconnect()`. `receive_response()` and `receive_messages()` return a
`MessageStream` (a `BoxStream` of `Result<Message>`) that does not borrow the
client; `collect_response()` / `collect_messages()` gather them into a `Vec`.

```rust,no_run
use claude_code_sdk_rust::{ClaudeAgentClient, ClaudeAgentOptions, Message, PermissionMode};
use futures::StreamExt;

#[tokio::main]
async fn main() -> claude_code_sdk_rust::Result<()> {
    let options = ClaudeAgentOptions::builder()
        .permission_mode(PermissionMode::AcceptEdits)
        .build();

    let mut client = ClaudeAgentClient::new(options)?;
    client.connect().await?;

    client.query("Summarize this repository's test strategy.").await?;
    let mut stream = client.receive_response();
    while let Some(message) = stream.next().await {
        if let Message::ResultMsg { result, .. } = message? {
            println!("{result:?}");
        }
    }

    // Or collect a whole turn at once.
    client.query("Now list the slowest tests.").await?;
    let turn = client.collect_response().await?;
    println!("{} messages", turn.len());

    client.disconnect().await?;
    Ok(())
}
```

Control methods take `&self`: `interrupt`, `set_permission_mode`, `set_model`,
`rewind_files`, `reconnect_mcp_server`, `toggle_mcp_server`, `stop_task`,
`get_mcp_status`, `get_context_usage` and `get_server_info`. Prompts accept
`impl Into<UserMessageInput>`, so text and images can be combined:

```rust,no_run
use claude_code_sdk_rust::{ClaudeAgentClient, ImageSource, InputContentBlock};

async fn ask_about_image(client: &ClaudeAgentClient) -> claude_code_sdk_rust::Result<()> {
    let blocks = vec![
        InputContentBlock::text("What is in this picture?"),
        InputContentBlock::image(ImageSource::Url {
            url: "https://example.com/cat.png".into(),
        }),
    ];
    client.query(blocks).await
}
```

### Event streaming

`stream_message` sends a prompt and returns a channel of high-level
`StreamEvent`s (text chunks, tool calls, results) for the turn.

```rust,no_run
use claude_code_sdk_rust::{ClaudeAgentClient, ClaudeAgentOptions, StreamEvent};

#[tokio::main]
async fn main() -> claude_code_sdk_rust::Result<()> {
    let options = ClaudeAgentOptions::builder()
        .include_partial_messages(true)
        .build();
    let mut client = ClaudeAgentClient::new(options)?;
    client.connect().await?;

    let mut events = client.stream_message("Write a haiku about compilers.").await?;
    while let Some(event) = events.recv().await {
        match event {
            StreamEvent::ContentChunk(text) => print!("{text}"),
            StreamEvent::ToolUseStart { name, .. } => println!("\n[tool: {name}]"),
            StreamEvent::TurnComplete(response) => {
                println!("\n[done: {:?}]", response.stop_reason);
                break;
            }
            StreamEvent::Error(error) => eprintln!("error: {error}"),
            _ => {}
        }
    }

    client.disconnect().await?;
    Ok(())
}
```

`ClaudeAgentClient::spawn_stream_message(options, prompt)` does the same
without managing a client yourself.

## Configuration

`ClaudeAgentOptions::builder()` covers the Python `ClaudeAgentOptions` fields.

```rust
use claude_code_sdk_rust::{
    ClaudeAgentOptions, EffortLevel, MCPServerConfig, PermissionMode, SettingSource,
    SystemPromptPreset, ThinkingConfig,
};

let options = ClaudeAgentOptions::builder()
    .cwd("/path/to/project")
    .model("claude-sonnet-4-5")
    .system_prompt_preset(
        SystemPromptPreset::claude_code()
            .with_append("Prefer small, reviewable diffs.")
            .with_exclude_dynamic_sections(true)
            .with_snapshot(true),
    )
    .allowed_tools(vec!["Read".into(), "Edit".into(), "Bash".into()])
    .permission_mode(PermissionMode::AcceptEdits)
    .setting_sources(vec![SettingSource::User, SettingSource::Project])
    .thinking(ThinkingConfig::adaptive())
    .effort(EffortLevel::High)
    .max_turns(20)
    .env_var("RUST_LOG", "info")
    .mcp_server(
        "docs",
        MCPServerConfig::Stdio {
            command: "docs-mcp".into(),
            args: Some(vec!["--stdio".into()]),
            env: None,
        },
    )
    .client_app("my-app/1.0")
    .verbatim_prompts(true)
    .forward_subagent_text(true)
    .stderr(|line| eprintln!("[claude] {line}"))
    .build();
```

Option areas include:

- CLI path, cwd, env, extra CLI args, `user`, `max_buffer_size`
- model, fallback model, betas, thinking, effort, task/USD budgets, output format
- system prompt: text, `SystemPromptPreset`, `SystemPromptCustom`
  (`system_prompt_custom`, with `with_snapshot`) or `SystemPromptFile`; the
  setters are mutually exclusive, like the Python union
- tools / tools preset, allowed/disallowed tools, permission mode,
  `can_use_tool`, `permission_prompt_tool_name`
- hooks (with `on_failure`) and hook event forwarding
- stdio/SSE/HTTP MCP servers and in-process SDK MCP servers
- agents, skills, plugins, sandbox settings, setting sources
- session `resume`, `continue_conversation`, `fork_session`, `session_id`,
  `resume_session_at(message_uuid)`, `resume_drops_turn(prompt_uuid)`, and
  session-store mirroring
- `client_app` (sent as `CLAUDE_AGENT_SDK_CLIENT_APP`), `verbatim_prompts`
  (no `@path` expansion or slash commands), `forward_subagent_text`
- stderr callback, file checkpointing, partial messages

## SDK MCP servers

Tools run in-process and are served to the CLI over the control protocol.
Handlers can be sync (`tool`) or async (`async_tool`, the Python `@tool`
decorator) and return an `SdkMcpToolResult`. Arguments are validated against
the input schema before the handler runs.

```rust
use claude_code_sdk_rust::{
    async_tool, async_tool_with_annotations, create_sdk_mcp_server, ClaudeAgentOptions,
    MCPToolAnnotations, SdkMcpToolResult,
};
use serde_json::json;

let greet = async_tool(
    "greet",
    "Greet a user",
    json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}),
    |args| async move {
        let name = args["name"].as_str().unwrap_or("there").to_string();
        SdkMcpToolResult::text(format!("Hello, {name}!"))
    },
);

let read_config = async_tool_with_annotations(
    "read_config",
    "Read the app config",
    json!({"type": "object", "properties": {}}),
    MCPToolAnnotations::new().read_only_hint(true).idempotent_hint(true),
    |_args| async move {
        match std::fs::read_to_string("config.toml") {
            Ok(text) => SdkMcpToolResult::text(text),
            Err(err) => SdkMcpToolResult::error(err.to_string()),
        }
    },
);

let server = create_sdk_mcp_server("local-tools", vec![greet, read_config]);

let options = ClaudeAgentOptions::builder()
    .sdk_mcp_server("local-tools", server)
    .allowed_tools(vec!["mcp__local-tools__greet".into()])
    .build();
```

`MCPToolAnnotations` hints are `Option<bool>`; only the hints you set are sent.
To handle raw JSON-RPC yourself, implement the `SdkMcpServer` trait and
register it with `sdk_mcp_custom_server(name, Arc::new(server))`.

## Permission callbacks

```rust
use claude_code_sdk_rust::{ClaudeAgentOptions, PermissionResult};

let options = ClaudeAgentOptions::builder()
    .can_use_tool(|tool_name, input, _context| async move {
        if tool_name == "Bash" {
            return Ok(PermissionResult::deny("Bash is disabled in this run"));
        }
        println!("allowing {tool_name} with {input:?}");
        Ok(PermissionResult::allow())
    })
    .build();
```

`can_use_tool` works with both the client and one-shot queries; the SDK keeps
the control channel open for the callback. It cannot be combined with
`permission_prompt_tool_name`, and a `tracing` warning is logged when
`allowed_tools` or `BypassPermissions` would shadow it, as in Python.

## Hooks

Hook callbacks receive the hook input as JSON and return JSON. The typed
`SyncHookJSONOutput` / `AsyncHookJSONOutput` and the per-event
`*HookSpecificOutput` structs convert into it.

```rust
use claude_code_sdk_rust::{
    ClaudeAgentOptions, HookCallback, HookEvent, HookMatcher, HookOnFailure,
    HookPermissionDecision, PreToolUseHookSpecificOutput, SyncHookJSONOutput,
};

let guard = HookCallback::new(|input, _tool_use_id, _context| async move {
    let command = input["tool_input"]["command"].as_str().unwrap_or_default();
    if !command.contains("rm -rf") {
        return Ok(SyncHookJSONOutput::default().into());
    }
    Ok(SyncHookJSONOutput {
        hook_specific_output: Some(
            PreToolUseHookSpecificOutput {
                permission_decision: Some(HookPermissionDecision::Deny),
                permission_decision_reason: Some("destructive command".into()),
                ..Default::default()
            }
            .into(),
        ),
        ..Default::default()
    }
    .into())
});

let options = ClaudeAgentOptions::builder()
    .hook(
        HookEvent::PreToolUse,
        HookMatcher::new(guard)
            .matcher("Bash")
            .on_failure(HookOnFailure::Block),
    )
    .include_hook_events(true)
    .build();
```

`on_failure(HookOnFailure::Block)` makes a failing callback block the action
instead of counting as no decision.

## Sessions

Local transcript helpers read `~/.claude/projects` and return the Python
shapes: `SDKSessionInfo` and `SessionMessage`.

```rust,no_run
use claude_code_sdk_rust::{
    get_session_info, get_session_messages, list_sessions, ListSessionsOptions,
    SessionQueryOptions,
};

#[tokio::main]
async fn main() -> claude_code_sdk_rust::Result<()> {
    let options = ListSessionsOptions {
        limit: Some(10),
        ..Default::default()
    };
    for session in list_sessions(&options).await? {
        println!("{} {}", session.session_id, session.summary);
    }

    let id = "00000000-0000-0000-0000-000000000000";
    let query = SessionQueryOptions {
        session_id: id.to_string(),
        include_messages: true,
        directory: None,
        limit: None,
        offset: None,
    };
    if let Some(info) = get_session_info(id, &query).await? {
        println!("{info:?}");
    }
    for message in get_session_messages(id, &query).await? {
        println!("{} {}", message.r#type, message.uuid);
    }
    Ok(())
}
```

Also available: `list_subagents` / `get_subagent_messages`, `rename_session`,
`tag_session`, `fork_session`, `delete_session`, `import_session_to_store`,
and `*_from_store` / `*_via_store` variants for any `SessionStore`.

## Custom session stores

Implement `SessionStore` to mirror transcripts to external storage
(`ClaudeAgentOptions::builder().session_store(store)`). Only `append` and
`load` are required; listing, summaries, subkeys and delete are opt-in.
`InMemorySessionStore` is included for tests.

```rust
use async_trait::async_trait;
use claude_code_sdk_rust::{Result, SessionKey, SessionStore, SessionStoreEntry};

struct MyStore;

#[async_trait]
impl SessionStore for MyStore {
    async fn append(&self, key: SessionKey, entries: Vec<SessionStoreEntry>) -> Result<()> {
        println!("append {} entries to {key:?}", entries.len());
        Ok(())
    }

    async fn load(&self, key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        println!("load {key:?}");
        Ok(None)
    }
}
```

Store adapters that keep summaries can call `fold_session_summary` inside
`append`.

## Error handling

Functions return `claude_code_sdk_rust::Result<T>` (`Result<T, ClaudeSDKError>`).
Variants cover CLI discovery (`CLINotFoundError`), connection failures,
process exits (`ProcessError`), JSON decoding and message parsing. When a run
ends with an error result, the error is a `ResultError` carrying the result
`subtype`, `errors`, `api_error_status`, `terminal_reason` and raw payload.

```rust
use claude_code_sdk_rust::ClaudeSDKError;

fn report(error: &ClaudeSDKError) {
    match error {
        ClaudeSDKError::Result(result) => {
            eprintln!("run failed ({:?}): {:?}", result.subtype, result.errors)
        }
        other if other.is_connection_error() => eprintln!("cannot reach the CLI: {other}"),
        other => eprintln!("error: {other} (exit code {:?})", other.exit_code()),
    }
}
```

## Custom transports

`Transport` is exported for alternative process or network transports;
`SubprocessCLITransport` is the default. Pass one to
`ClaudeAgentClient::with_transport` or the `*_with_transport` query functions.

## Testing

```bash
cargo test
cargo test --features otel
```

The README examples are compiled as doctests. The normal suite never calls
the paid API; the ignored real-CLI tests run only when the CLI is
authenticated:

```bash
cargo test --test e2e_cli -- --ignored
```

## License

MIT
