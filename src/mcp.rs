//! MCP (Model Context Protocol) support for Claude Agent SDK.

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

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
    /// HTTP-based MCP server.
    #[serde(rename = "http")]
    Http {
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

/// Hints about a tool's behavior, plus `maxResultSizeChars`.
///
/// Mirrors the Python SDK's `ToolAnnotations`: every hint is optional and
/// only the hints that were set reach the wire (under MCP, an absent
/// `destructiveHint` or `openWorldHint` means `true`, so leaving a hint unset
/// is different from setting it to `false`). Deserialization accepts both the
/// camelCase wire names and the snake_case spellings.
///
/// `max_result_size_chars` is not an MCP hint but a Claude Code one: the
/// size, in characters, up to which Claude Code keeps this tool's result
/// inline instead of persisting it to a file. In `tools/list` it travels to
/// the CLI in the tool's `_meta` as `anthropic/maxResultSizeChars` (and is
/// left out of the listed `annotations`, as in the Python SDK).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(
        default,
        alias = "read_only_hint",
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only_hint: Option<bool>,
    #[serde(
        default,
        alias = "destructive_hint",
        skip_serializing_if = "Option::is_none"
    )]
    pub destructive_hint: Option<bool>,
    #[serde(
        default,
        alias = "idempotent_hint",
        skip_serializing_if = "Option::is_none"
    )]
    pub idempotent_hint: Option<bool>,
    #[serde(
        default,
        alias = "open_world_hint",
        skip_serializing_if = "Option::is_none"
    )]
    pub open_world_hint: Option<bool>,
    #[serde(
        default,
        alias = "max_result_size_chars",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_result_size_chars: Option<usize>,
}

impl MCPToolAnnotations {
    /// Empty annotations (no hint set).
    pub fn new() -> Self {
        Self::default()
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn read_only_hint(mut self, value: bool) -> Self {
        self.read_only_hint = Some(value);
        self
    }

    pub fn destructive_hint(mut self, value: bool) -> Self {
        self.destructive_hint = Some(value);
        self
    }

    pub fn idempotent_hint(mut self, value: bool) -> Self {
        self.idempotent_hint = Some(value);
        self
    }

    pub fn open_world_hint(mut self, value: bool) -> Self {
        self.open_world_hint = Some(value);
        self
    }

    pub fn max_result_size_chars(mut self, value: usize) -> Self {
        self.max_result_size_chars = Some(value);
        self
    }
}

/// Content block returned by an SDK MCP tool.
///
/// On the wire (see the Python SDK's `_convert_tool_content`), text and image
/// blocks pass through, resource links and text resources are flattened to
/// text because that is what the CLI renders, and binary resources and audio
/// are dropped with a warning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MCPContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType", alias = "mime_type")]
        mime_type: String,
    },
    #[serde(rename = "audio")]
    Audio {
        data: String,
        #[serde(rename = "mimeType", alias = "mime_type")]
        mime_type: String,
    },
    #[serde(rename = "resource_link")]
    ResourceLink {
        uri: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default, rename = "mimeType", alias = "mime_type")]
        mime_type: Option<String>,
    },
    #[serde(rename = "resource")]
    Resource {
        uri: String,
        #[serde(default, rename = "mimeType", alias = "mime_type")]
        mime_type: Option<String>,
        #[serde(default)]
        text: Option<String>,
    },
}

impl MCPContent {
    /// A text content block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// A base64 image content block.
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::Image {
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }
}

/// The result of an SDK MCP tool call: content plus an error flag.
///
/// The Rust form of the Python handler's `{"content": [...], "is_error": bool}`
/// return value. It is sent to the CLI as `{"content": [...], "isError": bool}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SdkMcpToolResult {
    #[serde(default)]
    pub content: Vec<MCPContent>,
    #[serde(default, alias = "isError")]
    pub is_error: bool,
}

impl SdkMcpToolResult {
    /// A successful result with the given content.
    pub fn new(content: Vec<MCPContent>) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    /// A successful result with one text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![MCPContent::text(text)])
    }

    /// An error result (`isError: true`) with one text block.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![MCPContent::text(text)],
            is_error: true,
        }
    }

    /// Mark this result as an error (or not).
    pub fn with_is_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }
}

impl From<Vec<MCPContent>> for SdkMcpToolResult {
    fn from(content: Vec<MCPContent>) -> Self {
        Self::new(content)
    }
}

impl From<MCPContent> for SdkMcpToolResult {
    fn from(content: MCPContent) -> Self {
        Self::new(vec![content])
    }
}

/// A handler error (an `Err` return) becomes an `isError` text result, the
/// way the Python SDK reports an exception raised by a tool.
impl<T, E> From<std::result::Result<T, E>> for SdkMcpToolResult
where
    T: Into<SdkMcpToolResult>,
    E: std::fmt::Display,
{
    fn from(result: std::result::Result<T, E>) -> Self {
        match result {
            Ok(value) => value.into(),
            Err(error) => Self::error(error.to_string()),
        }
    }
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

/// The async handler behind an SDK MCP tool.
pub type SdkMcpToolHandler = dyn Fn(Value) -> BoxFuture<'static, SdkMcpToolResult> + Send + Sync;

fn sync_handler<F>(handler: F) -> Arc<SdkMcpToolHandler>
where
    F: Fn(Value) -> Result<Vec<MCPContent>, String> + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    Arc::new(move |input| {
        let handler = Arc::clone(&handler);
        // Run inside the future so a panic is caught like an async one.
        async move { SdkMcpToolResult::from(handler(input)) }.boxed()
    })
}

fn async_handler<F, Fut, R>(handler: F) -> Arc<SdkMcpToolHandler>
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: Into<SdkMcpToolResult>,
{
    Arc::new(move |input| {
        let future = handler(input);
        async move { future.await.into() }.boxed()
    })
}

/// Definition for an SDK MCP tool.
#[derive(Clone)]
pub struct SdkMcpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Option<MCPToolAnnotations>,
    handler: Arc<SdkMcpToolHandler>,
    validate_input: bool,
}

impl std::fmt::Debug for SdkMcpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkMcpTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .field("annotations", &self.annotations)
            .finish_non_exhaustive()
    }
}

impl SdkMcpTool {
    /// Build a tool from a synchronous handler. An `Err(message)` is reported
    /// to Claude as an `isError` text result.
    pub fn new<F>(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        annotations: Option<MCPToolAnnotations>,
        handler: F,
    ) -> Self
    where
        F: Fn(Value) -> Result<Vec<MCPContent>, String> + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            annotations,
            handler: sync_handler(handler),
            validate_input: true,
        }
    }

    /// Build a tool from an async handler, like the Python SDK's `@tool`.
    ///
    /// The handler's output is anything convertible into an
    /// [`SdkMcpToolResult`]: a result, a `Vec<MCPContent>`, or a `Result` of
    /// either whose `Err` is reported as an `isError` text result.
    pub fn new_async<F, Fut, R>(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        annotations: Option<MCPToolAnnotations>,
        handler: F,
    ) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = R> + Send + 'static,
        R: Into<SdkMcpToolResult>,
    {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            annotations,
            handler: async_handler(handler),
            validate_input: true,
        }
    }

    /// Run this tool's handler directly (no schema validation), like awaiting
    /// `tool.handler(args)` in Python. A panicking handler is reported as an
    /// `isError` result.
    pub async fn call(&self, input: Value) -> SdkMcpToolResult {
        crate::internal::sdk_mcp::run_handler(&self.handler, input).await
    }

    /// Whether `tools/call` arguments are checked against `input_schema`
    /// before the handler runs (default `true`, like the Python SDK). Pass
    /// `false` for a handler that accepts or repairs arguments outside its
    /// advertised schema; the MCP Python server's `validate_input=False`.
    pub fn validate_input(mut self, validate: bool) -> Self {
        self.validate_input = validate;
        self
    }

    fn into_parts(self) -> (MCPTool, Arc<SdkMcpToolHandler>, bool) {
        let tool = MCPTool {
            name: self.name,
            description: self.description,
            input_schema: self.input_schema,
            annotations: self.annotations,
        };
        (tool, self.handler, self.validate_input)
    }
}

/// Construct an SDK MCP tool definition from a synchronous handler.
pub fn tool<F>(
    name: impl Into<String>,
    description: impl Into<String>,
    input_schema: Value,
    handler: F,
) -> SdkMcpTool
where
    F: Fn(Value) -> Result<Vec<MCPContent>, String> + Send + Sync + 'static,
{
    SdkMcpTool::new(name, description, input_schema, None, handler)
}

/// Construct an SDK MCP tool definition from a synchronous handler, with MCP
/// annotations.
pub fn tool_with_annotations<F>(
    name: impl Into<String>,
    description: impl Into<String>,
    input_schema: Value,
    annotations: MCPToolAnnotations,
    handler: F,
) -> SdkMcpTool
where
    F: Fn(Value) -> Result<Vec<MCPContent>, String> + Send + Sync + 'static,
{
    SdkMcpTool::new(name, description, input_schema, Some(annotations), handler)
}

/// Construct an SDK MCP tool definition from an async handler (the Python
/// SDK's `@tool` decorator).
///
/// ```no_run
/// use claude_code_sdk_rust::mcp::{async_tool, SdkMcpToolResult};
///
/// let greet = async_tool(
///     "greet",
///     "Greet a user",
///     serde_json::json!({"name": "string"}),
///     |args| async move {
///         let name = args["name"].as_str().unwrap_or("there").to_string();
///         SdkMcpToolResult::text(format!("Hello, {name}!"))
///     },
/// );
/// ```
pub fn async_tool<F, Fut, R>(
    name: impl Into<String>,
    description: impl Into<String>,
    input_schema: Value,
    handler: F,
) -> SdkMcpTool
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: Into<SdkMcpToolResult>,
{
    SdkMcpTool::new_async(name, description, input_schema, None, handler)
}

/// Construct an SDK MCP tool definition from an async handler, with MCP
/// annotations.
pub fn async_tool_with_annotations<F, Fut, R>(
    name: impl Into<String>,
    description: impl Into<String>,
    input_schema: Value,
    annotations: MCPToolAnnotations,
    handler: F,
) -> SdkMcpTool
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: Into<SdkMcpToolResult>,
{
    SdkMcpTool::new_async(name, description, input_schema, Some(annotations), handler)
}

/// A user-supplied in-process MCP server (the Python SDK's "bring your own
/// `mcp.server.Server`").
///
/// It receives every JSON-RPC message the CLI sends to the server
/// (`initialize`, `ping`, `tools/*`, `resources/*`, `prompts/*`, ...) and
/// answers requests itself. Return `Ok(None)` for notifications and
/// responses; the SDK acknowledges them to the CLI. An `Err(message)` (or a
/// panic) answers the request with JSON-RPC error `-32603`. Duplicate
/// in-flight request ids and `notifications/cancelled` are handled by the
/// SDK: a cancelled request is answered with `-32800 Request cancelled`, and
/// the notification is still forwarded to the server.
///
/// Wrap one with [`SimpleMCPServer::from_server`] to register it with
/// `ClaudeAgentOptionsBuilder::sdk_mcp_server`.
#[async_trait]
pub trait SdkMcpServer: Send + Sync {
    /// Handle one JSON-RPC message from the CLI.
    async fn handle_message(&self, message: Value) -> Result<Option<Value>, String>;
}

#[async_trait]
impl<T: SdkMcpServer + ?Sized> SdkMcpServer for Arc<T> {
    async fn handle_message(&self, message: Value) -> Result<Option<Value>, String> {
        (**self).handle_message(message).await
    }
}

pub(crate) struct RegisteredTool {
    /// The tool as listed on the wire (input schema already normalized).
    pub(crate) tool: MCPTool,
    pub(crate) handler: Arc<SdkMcpToolHandler>,
    pub(crate) validate_input: bool,
}

impl Clone for RegisteredTool {
    fn clone(&self) -> Self {
        Self {
            tool: self.tool.clone(),
            handler: Arc::clone(&self.handler),
            validate_input: self.validate_input,
        }
    }
}

/// An in-process SDK MCP server: a list of tools served over the CLI's
/// control channel, or a wrapped user-supplied [`SdkMcpServer`].
///
/// Tools are listed in registration order. Cloning a server shares its tools
/// but not its table of in-flight requests.
pub struct SimpleMCPServer {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) tools: Vec<RegisteredTool>,
    pub(crate) custom: Option<Arc<dyn SdkMcpServer>>,
    pub(crate) in_flight: crate::internal::sdk_mcp::InFlightTable,
}

impl Clone for SimpleMCPServer {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            version: self.version.clone(),
            tools: self.tools.clone(),
            custom: self.custom.clone(),
            in_flight: Default::default(),
        }
    }
}

impl std::fmt::Debug for SimpleMCPServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimpleMCPServer")
            .field("name", &self.name)
            .field("version", &self.version)
            .field(
                "tools",
                &self
                    .tools
                    .iter()
                    .map(|entry| entry.tool.name.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("custom", &self.custom.is_some())
            .finish()
    }
}

impl SimpleMCPServer {
    /// Create a new simple MCP server.
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_version(name, "1.0.0")
    }

    /// Create a new simple MCP server with an explicit version.
    pub fn with_version(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools: Vec::new(),
            custom: None,
            in_flight: Default::default(),
        }
    }

    /// Serve a user-supplied [`SdkMcpServer`] under `name`. Every message the
    /// CLI sends to this server is passed to it.
    pub fn from_server(name: impl Into<String>, server: Arc<dyn SdkMcpServer>) -> Self {
        let mut wrapped = Self::new(name);
        wrapped.custom = Some(server);
        wrapped
    }

    /// Whether this wraps a user-supplied [`SdkMcpServer`].
    pub fn is_custom(&self) -> bool {
        self.custom.is_some()
    }

    fn insert(&mut self, tool: MCPTool, handler: Arc<SdkMcpToolHandler>, validate_input: bool) {
        let tool = MCPTool {
            input_schema: crate::internal::sdk_mcp::build_input_schema(&tool.input_schema),
            ..tool
        };
        let entry = RegisteredTool {
            tool,
            handler,
            validate_input,
        };
        // A later registration under the same name replaces the earlier one
        // in place, keeping the listing order.
        if let Some(existing) = self
            .tools
            .iter_mut()
            .find(|existing| existing.tool.name == entry.tool.name)
        {
            *existing = entry;
        } else {
            self.tools.push(entry);
        }
    }

    /// Register a tool with a synchronous handler.
    pub fn register_tool<F>(&mut self, tool: MCPTool, handler: F)
    where
        F: Fn(Value) -> Result<Vec<MCPContent>, String> + Send + Sync + 'static,
    {
        self.insert(tool, sync_handler(handler), true);
    }

    /// Register a tool with an async handler.
    pub fn register_async_tool<F, Fut, R>(&mut self, tool: MCPTool, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = R> + Send + 'static,
        R: Into<SdkMcpToolResult>,
    {
        self.insert(tool, async_handler(handler), true);
    }

    /// Register a pre-built SDK MCP tool with the server.
    pub fn register_sdk_tool(&mut self, tool: SdkMcpTool) {
        let (tool, handler, validate_input) = tool.into_parts();
        self.insert(tool, handler, validate_input);
    }

    /// Get all registered tools, in registration order, with their input
    /// schemas as sent on the wire.
    pub fn list_tools(&self) -> Vec<&MCPTool> {
        self.tools.iter().map(|entry| &entry.tool).collect()
    }

    /// Call a tool by name the way a `tools/call` request does: unknown tools,
    /// arguments that fail input-schema validation, handler errors and panics
    /// all come back as `isError` results.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> SdkMcpToolResult {
        crate::internal::sdk_mcp::run_tool(self, name, arguments).await
    }

    /// Handle one raw JSON-RPC message from the CLI. Returns the response for
    /// requests and `None` for notifications and responses.
    pub async fn handle_message(&self, message: &Value) -> Option<Value> {
        crate::internal::sdk_mcp::handle_message(self, message).await
    }

    /// Get the server name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the server version.
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// Initialize an MCP server.
pub fn initialize_server(name: impl Into<String>) -> SimpleMCPServer {
    SimpleMCPServer::new(name)
}

/// Create an in-process SDK MCP server with pre-registered tools.
pub fn create_sdk_mcp_server(name: impl Into<String>, tools: Vec<SdkMcpTool>) -> SimpleMCPServer {
    create_sdk_mcp_server_with_version(name, "1.0.0", tools)
}

/// Create an in-process SDK MCP server with an explicit version.
pub fn create_sdk_mcp_server_with_version(
    name: impl Into<String>,
    version: impl Into<String>,
    tools: Vec<SdkMcpTool>,
) -> SimpleMCPServer {
    let mut server = SimpleMCPServer::with_version(name, version);
    for tool in tools {
        server.register_sdk_tool(tool);
    }
    server
}
