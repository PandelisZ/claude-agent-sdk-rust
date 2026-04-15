pub mod types;
pub mod error;
pub mod options;
pub mod query;
pub mod client;
pub mod sessions;
pub mod mcp;

pub mod internal {
    pub mod transport;
    pub mod protocol;
    pub mod runtime;
    pub mod parser;
    pub mod sessions_fs;
    pub mod message_parser;
}

// Re-export commonly used types
pub use types::*;

// Re-export error types
pub use error::{
    ClaudeSDKError, Result, CLIConnectionError, CLINotFoundError, ProcessError,
    CLIJSONDecodeError, MessageParseError,
};

// Re-export options types
pub use options::{
    ClaudeAgentOptionsBuilder,
    ListSessionsOptions,
    SessionQueryOptions,
    SessionMutationOptions,
};

// Re-export query function and types
pub use query::{
    query,
    QueryResult,
    TokenUsage,
};

// Re-export client types
pub use client::{ClaudeAgentClient, MessageResponse, StreamEvent};
