# Claude Agent SDK for Rust

A Rust SDK for driving a local Claude Code CLI process. This SDK provides a type-safe wrapper around the Claude CLI's JSON-RPC-like protocol.

## Overview

This SDK wraps the local Claude CLI (`claude` command) and communicates via line-delimited JSON. It provides:

- **Query API** - One-shot, stateless interactions
- **Client API** - Interactive, stateful sessions with bidirectional communication
- **Session Management** - List, read, and manage local Claude sessions
- **MCP Support** - Model Context Protocol server integration

## Prerequisites

- Rust 1.70+ (for async trait support)
- Claude CLI installed and authenticated (`claude` command available in PATH)

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
claude-agent-sdk = { git = "https://github.com/PandelisZ/claude-agent-sdk-rust" }
```

## Quick Start

### One-shot Query

```rust
use claude_agent_sdk::query;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result = query("What is Rust?", None).await?;
    println!("{}", result.content);
    Ok(())
}
```

### Interactive Client

```rust
use claude_agent_sdk::{ClaudeAgentClient, ClaudeAgentOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = ClaudeAgentOptions::builder()
        .model("claude-sonnet-4-20250514")
        .build();
    
    let mut client = ClaudeAgentClient::new(options)?;
    
    // Send a message and get the response
    let response = client.send_message("Hello, Claude!").await?;
    println!("Response: {}", response.content);
    
    Ok(())
}
```

### With Configuration

```rust
use claude_agent_sdk::ClaudeAgentOptions;
use std::collections::HashMap;

let options = ClaudeAgentOptions::builder()
    .cwd("/path/to/project")
    .model("claude-sonnet-4-20250514")
    .system_prompt("You are a helpful coding assistant")
    .mcp_server("my-server", MCPServerConfig::Stdio {
        command: "my-mcp-server".to_string(),
        args: Some(vec!["--port".to_string(), "8080".to_string()]),
        env: None,
    })
    .permission_mode(PermissionMode::AskOnFirstUse)
    .build();
```

## Features

### Core Types

- `Message` - All message types (UserMsg, AssistantMsg, SystemMsg, ResultMsg, etc.)
- `ContentBlock` - Content types (Text, Thinking, ToolUse, ToolResult)
- `ClaudeAgentOptions` - Configuration with builder pattern

### Error Handling

Comprehensive error hierarchy:
- `ClaudeSDKError` - Base error type
- `CLIConnectionError` - Connection failures
- `CLINotFoundError` - CLI binary not found
- `ProcessError` - Subprocess failures
- `CLIJSONDecodeError` - Malformed JSON from CLI

### Session Management

```rust
use claude_agent_sdk::sessions;

// List all sessions for a directory
let sessions = sessions::list_sessions(None).await?;

// Get session info
let info = sessions::get_session_info("session-uuid", None).await?;

// Get session messages
let messages = sessions::get_session_messages("session-uuid", None).await?;
```

## Architecture

This SDK is modeled after the Anthropic Python SDK but adapted for the CLI-wrapper pattern:

- **Async/Await** - Full async support with Tokio
- **Type Safety** - Comprehensive type system with serde
- **Builder Pattern** - Ergonomic configuration
- **Zero-copy** where possible with `bytes` crate

## License

MIT

## Contributing

Contributions welcome! Please ensure:
- `cargo test` passes
- `cargo clippy` produces no warnings
- Code is formatted with `cargo fmt`
