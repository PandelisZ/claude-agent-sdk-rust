//! Async Rust SDK for the Claude Code CLI, a port of the Python
//! [`claude-agent-sdk`](https://github.com/anthropics/claude-agent-sdk-python)
//! (parity target: v0.2.189).
//!
//! The SDK spawns an authenticated local `claude` CLI and talks to it over the
//! stream-json control protocol. Entry points:
//!
//! - [`query()`] / [`stream_query`]: one-shot runs (the Python `query()`);
//!   [`stream_query`] yields each [`Message`] as a [`MessageStream`], and
//!   [`query_messages`] collects them.
//! - [`ClaudeAgentClient`] (alias [`ClaudeSDKClient`]): an interactive session
//!   with [`receive_response`](ClaudeAgentClient::receive_response) /
//!   [`receive_messages`](ClaudeAgentClient::receive_messages) streams,
//!   [`collect_response`](ClaudeAgentClient::collect_response), event
//!   streaming and control requests (interrupt, model and permission-mode
//!   changes, MCP status, ...).
//! - [`ClaudeAgentOptions::builder`]: typed options, including hooks
//!   ([`HookMatcher`]), permission callbacks
//!   ([`ClaudeAgentOptionsBuilder::can_use_tool`]) and in-process SDK MCP
//!   servers ([`async_tool`], [`create_sdk_mcp_server`]).
//! - [`sessions`]: local transcript helpers ([`list_sessions`],
//!   [`get_session_messages`], ...) and [`SessionStore`]-backed variants.
//! - [`Transport`]: the transport extension point
//!   ([`SubprocessCLITransport`] by default).
//!
//! ```no_run
//! use claude_code_sdk_rust::{ClaudeAgentClient, ClaudeAgentOptions, Message};
//! use futures::StreamExt;
//!
//! # async fn run() -> claude_code_sdk_rust::Result<()> {
//! let mut client = ClaudeAgentClient::new(ClaudeAgentOptions::default())?;
//! client.connect().await?;
//! client.query("What does this repository do?").await?;
//! let mut messages = client.receive_response();
//! while let Some(message) = messages.next().await {
//!     if let Message::ResultMsg { result, .. } = message? {
//!         println!("{result:?}");
//!     }
//! }
//! client.disconnect().await?;
//! # Ok(())
//! # }
//! ```
//!
//! See the README for more examples; its code blocks are compiled as
//! doctests.

pub mod client;
mod client_stream;
pub mod client_types;
pub mod error;
pub mod mcp;
pub mod options;
pub mod query;
pub mod session_store;
pub mod session_summary;
pub mod sessions;
pub mod types;

/// SDK crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod internal {
    pub mod cli_args;
    #[cfg(test)]
    mod cli_args_tests;
    pub mod cli_discovery;
    pub mod client_app;
    pub mod control;
    pub mod message_parser;
    pub mod parser;
    pub mod protocol;
    pub mod runtime;
    pub mod sdk_mcp;
    pub mod session_resume;
    #[cfg(test)]
    mod session_resume_tests;
    pub mod session_store_validation;
    pub mod sessions_fs;
    mod stdout_decoder;
    pub mod transcript_mirror;
    pub mod transport;
}

// Re-export commonly used types
pub use types::*;

pub type McpServerInfo = types::MCPServerInfo;
pub type McpServerStatus = types::MCPServerStatus;
pub type McpStatusResponse = types::MCPStatusResponse;
pub type McpToolAnnotations = types::MCPToolAnnotations;
pub type McpToolInfo = types::MCPToolInfo;

// Re-export error types
pub use error::ResultError;
pub use error::{
    CLIConnectionError, CLIJSONDecodeError, CLINotFoundError, ClaudeSDKError, MessageParseError,
    ProcessError, Result,
};

// Re-export options types
pub use options::ClaudeAgentOptionsBuilder;

// Re-export query function and types
pub use query::{
    query, query_messages, query_messages_with_transport, query_stream_messages,
    query_stream_messages_with_transport, stream_query, stream_query_with_transport, QueryPrompt,
    QueryResult, TokenUsage,
};

/// A stream of parsed messages (see [`stream_query`] and
/// [`ClaudeAgentClient::receive_messages`]).
pub use internal::runtime::MessageStream;

// The transport extension point (Python exports `Transport` at the root).
pub use internal::transport::{SubprocessCLITransport, Transport, TransportOptions};

// Re-export client types
pub use client::ClaudeAgentClient;
pub use client_types::{MessageResponse, StreamEvent};

/// Python SDK-compatible alias for the interactive client.
pub type ClaudeSDKClient = ClaudeAgentClient;

// Re-export in-process MCP helpers
pub use mcp::{async_tool, async_tool_with_annotations, SdkMcpServer, SdkMcpToolResult};
pub use mcp::{
    create_sdk_mcp_server, create_sdk_mcp_server_with_version, initialize_server, tool,
    tool_with_annotations, MCPContent, MCPTool, MCPToolAnnotations, SdkMcpTool, SimpleMCPServer,
};

// Re-export session store and session helpers
pub use session_store::{
    project_key_for_directory, InMemorySessionStore, SessionKey, SessionListSubkeysKey,
    SessionStore, SessionStoreEntry, SessionStoreHandle, SessionStoreListEntry,
    SessionSummaryEntry,
};
pub use sessions::{
    delete_session, delete_session_via_store, fork_session, fork_session_via_store,
    get_session_info, get_session_info_from_store, get_session_messages,
    get_session_messages_from_store, get_subagent_messages, get_subagent_messages_from_store,
    import_session_to_store, list_sessions, list_sessions_from_store, list_subagents,
    list_subagents_from_store, rename_session, rename_session_via_store, tag_session,
    tag_session_via_store, ForkSessionResult, ImportSessionOptions, ListSessionsOptions,
    LocalForkSessionResult, SDKSessionInfo, SDKSessionMessage, SessionInfo, SessionMessage,
    SessionMutationOptions, SessionQueryOptions,
};

// Store adapters call this inside append() (Python exports it at the root).
pub use session_summary::fold_session_summary;

// Compile the README's code examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
