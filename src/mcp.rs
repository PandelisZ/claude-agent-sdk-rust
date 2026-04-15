//! MCP (Model Context Protocol) support for Claude Agent SDK.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Configuration for an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MCPServerConfig {
    /// Stdio-based MCP server.
    #[serde(rename = "stdio")]
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// SSE-based MCP server.
    #[serde(rename = "sse")]
    Sse {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

/// Information about an MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MCPTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Option<MCPToolAnnotations>,
}

/// Annotations for an MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MCPToolAnnotations {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub read_only_hint: bool,
    #[serde(default)]
    pub destructive_hint: bool,
    #[serde(default)]
    pub idempotent_hint: bool,
    #[serde(default)]
    pub open_world_hint: bool,
}

/// Content block for MCP tool results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MCPContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, mime_type: String },
    #[serde(rename = "resource")]
    Resource { uri: String, mime_type: Option<String>, text: Option<String> },
}

/// Status of an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MCPServerStatus {
    pub name: String,
    pub status: MCPConnectionStatus,
    #[serde(default)]
    pub tools: Vec<MCPTool>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Connection status of an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MCPConnectionStatus {
    Connected,
    Disconnected,
    Error,
}

/// A simple in-process MCP server.
pub struct SimpleMCPServer {
    name: String,
    tools: HashMap<String, MCPTool>,
    handlers: HashMap<String, Box<dyn Fn(Value) -> Result<Vec<MCPContent>, String> + Send + Sync>>,
}

impl SimpleMCPServer {
    /// Create a new simple MCP server.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tools: HashMap::new(),
            handlers: HashMap::new(),
        }
    }

    /// Register a tool with the server.
    pub fn register_tool<F>(&mut self, tool: MCPTool, handler: F)
    where
        F: Fn(Value) -> Result<Vec<MCPContent>, String> + Send + Sync + 'static,
    {
        let name = tool.name.clone();
        self.tools.insert(name.clone(), tool);
        self.handlers.insert(name, Box::new(handler));
    }

    /// Get all registered tools.
    pub fn list_tools(&self) -> Vec<&MCPTool> {
        self.tools.values().collect()
    }

    /// Call a tool by name.
    pub fn call_tool(&self, name: &str, input: Value) -> Result<Vec<MCPContent>, String> {
        if let Some(handler) = self.handlers.get(name) {
            handler(input)
        } else {
            Err(format!("Tool '{}' not found", name))
        }
    }

    /// Get the server name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Initialize an MCP server.
pub fn initialize_server(name: impl Into<String>) -> SimpleMCPServer {
    SimpleMCPServer::new(name)
}
