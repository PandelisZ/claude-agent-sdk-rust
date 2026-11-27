//! In-process (SDK) MCP servers: the JSON-RPC side of the CLI's `mcp_message`
//! control requests.
//!
//! Mirrors the Python SDK's `create_sdk_mcp_server` / `SdkMcpBridge` /
//! `Query._handle_sdk_mcp_request`, which delegate to a real `mcp` lowlevel
//! `Server`: `initialize` (with protocol-version negotiation), `ping`,
//! `tools/list`, `tools/call` (input-schema validation, `isError` results,
//! content conversion), notification acks, `notifications/cancelled`, and
//! refusing a request id that is already in flight.

use std::cell::Cell;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::mcp::{MCPContent, MCPTool, SdkMcpToolHandler, SdkMcpToolResult, SimpleMCPServer};
use futures::future::{AbortHandle, Abortable};
use futures::FutureExt;
use serde_json::{json, Map, Value};

/// Protocol revisions reachable via the `initialize` handshake (mcp
/// `HANDSHAKE_PROTOCOL_VERSIONS`).
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// What `initialize` answers when the client asks for a revision it does not
/// support (mcp `LATEST_HANDSHAKE_VERSION`).
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

/// JSON-RPC "method not found".
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC "invalid params".
pub const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC "internal error".
pub const INTERNAL_ERROR: i64 = -32603;
/// The error a request cancelled by the client is ended with.
pub const REQUEST_CANCELLED: i64 = -32800;

/// Error text of a control request without `server_name` or `message`.
pub const MISSING_SERVER_OR_MESSAGE: &str = "Missing server_name or message for MCP request";

// ---------------------------------------------------------------------------
// In-flight requests
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct InFlightEntry {
    token: u64,
    method: String,
    abort: AbortHandle,
}

/// Requests a server is currently answering, keyed by JSON-encoded id.
pub(crate) type InFlightTable = Arc<Mutex<HashMap<String, InFlightEntry>>>;

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

struct InFlightGuard {
    table: InFlightTable,
    key: String,
    token: u64,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        if table.get(&self.key).map(|entry| entry.token) == Some(self.token) {
            table.remove(&self.key);
        }
    }
}

fn id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_default()
}

fn is_valid_request_id(id: &Value) -> bool {
    id.is_string() || id.is_i64() || id.is_u64()
}

// ---------------------------------------------------------------------------
// Control-request entry points
// ---------------------------------------------------------------------------

/// Answer the `mcp_message` control request payload (`request` is the inner
/// `request` object of the control request).
///
/// Returns the control response body `{"mcp_response": ...}`, or the error
/// text for a control error response when `server_name` or `message` is
/// missing (as in the Python SDK's `_handle_control_request`).
pub async fn answer_mcp_control_request(
    servers: &HashMap<String, SimpleMCPServer>,
    request: &Map<String, Value>,
) -> Result<Value, String> {
    let server_name = request
        .get("server_name")
        .and_then(|v| v.as_str())
        .filter(|name| !name.is_empty());
    let message = request
        .get("message")
        .filter(|message| message.as_object().is_some_and(|m| !m.is_empty()));
    let (Some(server_name), Some(message)) = (server_name, message) else {
        return Err(MISSING_SERVER_OR_MESSAGE.to_string());
    };
    Ok(json!({
        "mcp_response": answer_mcp_message(servers, server_name, message).await
    }))
}

/// Route one JSON-RPC message from the CLI to the named SDK MCP server.
///
/// Returns the JSON-RPC response for requests; notifications and responses,
/// which get no JSON-RPC reply, are acknowledged with
/// `{"jsonrpc": "2.0", "result": {}}` because the control request carrying
/// them still expects one.
pub async fn answer_mcp_message(
    servers: &HashMap<String, SimpleMCPServer>,
    server_name: &str,
    message: &Value,
) -> Value {
    let Some(server) = servers.get(server_name) else {
        return jsonrpc_error(
            message.get("id").cloned().unwrap_or(Value::Null),
            METHOD_NOT_FOUND,
            &format!("Server '{server_name}' not found"),
        );
    };
    handle_message(server, message)
        .await
        .unwrap_or_else(|| json!({"jsonrpc": "2.0", "result": {}}))
}

/// Handle one raw JSON-RPC message for `server`. `None` for notifications and
/// responses.
pub(crate) async fn handle_message(server: &SimpleMCPServer, message: &Value) -> Option<Value> {
    let Some(object) = message.as_object() else {
        return Some(jsonrpc_error(
            Value::Null,
            INTERNAL_ERROR,
            "Invalid JSON-RPC message: expected an object",
        ));
    };
    let method = object.get("method").and_then(|v| v.as_str());
    let id = object.get("id").filter(|id| is_valid_request_id(id));

    let Some(method) = method else {
        if object.contains_key("result") || object.contains_key("error") {
            // A response to a request we never send: nothing to answer.
            return None;
        }
        return Some(jsonrpc_error(
            object.get("id").cloned().unwrap_or(Value::Null),
            INTERNAL_ERROR,
            "Invalid JSON-RPC message: expected a request, notification or response",
        ));
    };

    let Some(id) = id else {
        handle_notification(server, method, message).await;
        return None;
    };
    handle_request(server, id.clone(), method, message).await
}

async fn handle_notification(server: &SimpleMCPServer, method: &str, message: &Value) {
    if method == "notifications/cancelled" {
        if let Some(request_id) = message.pointer("/params/requestId") {
            let table = server.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = table.get(&id_key(request_id)) {
                // The handshake itself is never cancelled.
                if entry.method != "initialize" {
                    entry.abort.abort();
                }
            }
        }
    }
    if let Some(custom) = &server.custom {
        let forwarded = AssertUnwindSafe(custom.handle_message(message.clone()))
            .catch_unwind()
            .await;
        match forwarded {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::debug!(
                server = %server.name,
                method,
                %error,
                "SDK MCP server failed to handle a notification"
            ),
            Err(_) => tracing::debug!(
                server = %server.name,
                method,
                "SDK MCP server panicked handling a notification"
            ),
        }
    }
}

async fn handle_request(
    server: &SimpleMCPServer,
    id: Value,
    method: &str,
    message: &Value,
) -> Option<Value> {
    let key = id_key(&id);
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let (abort, registration) = AbortHandle::new_pair();
    {
        let mut table = server.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        if table.contains_key(&key) {
            return Some(jsonrpc_error(
                id.clone(),
                INTERNAL_ERROR,
                &format!("Request id {} is already in flight", py_repr(&id)),
            ));
        }
        table.insert(
            key.clone(),
            InFlightEntry {
                token,
                method: method.to_string(),
                abort,
            },
        );
    }
    let _guard = InFlightGuard {
        table: Arc::clone(&server.in_flight),
        key,
        token,
    };

    let dispatch = async {
        match &server.custom {
            Some(custom) => match custom.handle_message(message.clone()).await {
                Ok(Some(response)) => Some(response),
                // A request the server settles without answering is ended
                // the way mcp 2 ends it.
                Ok(None) => Some(cancelled_response(&id)),
                Err(error) => Some(jsonrpc_error(id.clone(), INTERNAL_ERROR, &error)),
            },
            None => Some(match dispatch_builtin(server, method, message).await {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, text)) => jsonrpc_error(id.clone(), code, &text),
            }),
        }
    };
    match Abortable::new(AssertUnwindSafe(dispatch).catch_unwind(), registration).await {
        Ok(Ok(response)) => response,
        Ok(Err(panic)) => Some(jsonrpc_error(
            id.clone(),
            INTERNAL_ERROR,
            &panic_message(panic.as_ref()),
        )),
        Err(_aborted) => Some(cancelled_response(&id)),
    }
}

fn cancelled_response(id: &Value) -> Value {
    jsonrpc_error(id.clone(), REQUEST_CANCELLED, "Request cancelled")
}

async fn dispatch_builtin(
    server: &SimpleMCPServer,
    method: &str,
    message: &Value,
) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let requested = message
                .pointer("/params/protocolVersion")
                .and_then(|v| v.as_str());
            let protocol_version = requested
                .filter(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(version))
                .unwrap_or(LATEST_PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": protocol_version,
                "capabilities": {
                    "experimental": {},
                    "tools": {"listChanged": false},
                },
                "serverInfo": {
                    "name": server.name(),
                    "version": server.version(),
                },
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": server
                .list_tools()
                .into_iter()
                .map(tool_to_wire)
                .collect::<Vec<_>>(),
        })),
        "tools/call" => {
            let params = message.get("params").and_then(|v| v.as_object());
            let name = params
                .and_then(|params| params.get("name"))
                .and_then(|v| v.as_str());
            let arguments = params.and_then(|params| params.get("arguments"));
            let arguments = match arguments {
                None | Some(Value::Null) => Value::Object(Map::new()),
                Some(Value::Object(arguments)) => Value::Object(arguments.clone()),
                Some(_) => {
                    return Err((INVALID_PARAMS, "Invalid request parameters".to_string()));
                }
            };
            let Some(name) = name else {
                return Err((INVALID_PARAMS, "Invalid request parameters".to_string()));
            };
            let result = run_tool(server, name, arguments).await;
            Ok(result_to_wire(result))
        }
        _ => Err((METHOD_NOT_FOUND, "Method not found".to_string())),
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// Run a tool the way `tools/call` does (Python `run_tool`): unknown tools,
/// invalid arguments, handler errors and panics are all `isError` results.
pub(crate) async fn run_tool(
    server: &SimpleMCPServer,
    name: &str,
    arguments: Value,
) -> SdkMcpToolResult {
    let Some(entry) = server.tools.iter().find(|entry| entry.tool.name == name) else {
        return SdkMcpToolResult::error(format!("Tool '{name}' not found"));
    };
    match validate_arguments(&arguments, &entry.tool.input_schema) {
        Ok(()) => {}
        Err(ValidationFailure::Schema(message)) => return SdkMcpToolResult::error(message),
        Err(ValidationFailure::Instance(message)) => {
            return SdkMcpToolResult::error(format!("Input validation error: {message}"));
        }
    }
    run_handler(&entry.handler, arguments).await
}

/// Await a tool handler, turning a panic into an `isError` result.
pub(crate) async fn run_handler(
    handler: &Arc<SdkMcpToolHandler>,
    input: Value,
) -> SdkMcpToolResult {
    let future = match std::panic::catch_unwind(AssertUnwindSafe(|| handler(input))) {
        Ok(future) => future,
        Err(panic) => return SdkMcpToolResult::error(panic_message(panic.as_ref())),
    };
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => result,
        Err(panic) => SdkMcpToolResult::error(panic_message(panic.as_ref())),
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "tool handler panicked".to_string()
    }
}

/// The `CallToolResult` sent to the CLI: `{"content": [...], "isError": bool}`.
pub fn result_to_wire(result: SdkMcpToolResult) -> Value {
    json!({
        "content": convert_tool_content(result.content),
        "isError": result.is_error,
    })
}

/// Map a tool's content to the MCP content blocks the CLI renders (Python
/// `_convert_tool_content`).
pub fn convert_tool_content(content: Vec<MCPContent>) -> Vec<Value> {
    let mut converted = Vec::with_capacity(content.len());
    for item in content {
        match item {
            MCPContent::Text { text } => converted.push(json!({"type": "text", "text": text})),
            MCPContent::Image { data, mime_type } => {
                converted.push(json!({"type": "image", "data": data, "mimeType": mime_type}));
            }
            MCPContent::ResourceLink {
                uri,
                name,
                description,
                ..
            } => {
                let parts: Vec<String> = [name, Some(uri), description]
                    .into_iter()
                    .flatten()
                    .filter(|part| !part.is_empty())
                    .collect();
                let text = if parts.is_empty() {
                    "Resource link".to_string()
                } else {
                    parts.join("\n")
                };
                converted.push(json!({"type": "text", "text": text}));
            }
            MCPContent::Resource {
                text: Some(text), ..
            } => {
                converted.push(json!({"type": "text", "text": text}));
            }
            MCPContent::Resource { text: None, .. } => {
                tracing::warn!("Binary embedded resource cannot be converted to text, skipping");
            }
            MCPContent::Audio { .. } => {
                tracing::warn!("Unsupported content type 'audio' in tool result, skipping");
            }
        }
    }
    converted
}

/// A tool as listed in `tools/list`: only the annotation hints that were set,
/// with `maxResultSizeChars` carried in `_meta`.
pub fn tool_to_wire(tool: &MCPTool) -> Value {
    let mut value = json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": tool.input_schema,
    });
    if let Some(annotations) = &tool.annotations {
        let mut listed = annotations.clone();
        let max_size = listed.max_result_size_chars.take();
        value["annotations"] = serde_json::to_value(&listed).unwrap_or_else(|_| json!({}));
        if let Some(max_size) = max_size {
            value["_meta"] = json!({"anthropic/maxResultSizeChars": max_size});
        }
    }
    value
}

/// Turn a tool's declared input schema into the JSON Schema sent on the wire
/// (Python `_build_input_schema`).
///
/// - A JSON Schema (an object with a string `type` and `properties`) is sent
///   verbatim. As a Rust-side allowance, `{"type": "object", ...}` without
///   `properties` is also treated as a JSON Schema, with `properties: {}`
///   added (Python would read it as a one-parameter map).
/// - Any other object is a `{param: type}` map: each value is a JSON type name
///   (`"string"`, `"integer"`, `"number"`, `"boolean"`, `"array"`,
///   `"object"`, `"null"`) or a JSON Schema object for that parameter, and
///   every parameter is required. `{}` becomes
///   `{"type": "object", "properties": {}, "required": []}`.
/// - A non-object becomes `{"type": "object", "properties": {}}`.
pub fn build_input_schema(input_schema: &Value) -> Value {
    let Some(schema) = input_schema.as_object() else {
        return json!({"type": "object", "properties": {}});
    };
    let schema_type = schema.get("type").and_then(|v| v.as_str());
    if schema_type.is_some() && schema.contains_key("properties") {
        return input_schema.clone();
    }
    if schema_type == Some("object") {
        let mut schema = schema.clone();
        schema.insert("properties".to_string(), json!({}));
        return Value::Object(schema);
    }
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (param, param_type) in schema {
        let property = match param_type {
            Value::String(name) if JSON_TYPES.contains(&name.as_str()) => json!({"type": name}),
            Value::Object(_) => param_type.clone(),
            _ => json!({"type": "string"}),
        };
        properties.insert(param.clone(), property);
        required.push(Value::String(param.clone()));
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

fn jsonrpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

// ---------------------------------------------------------------------------
// Argument validation (a JSON Schema subset, Python `jsonschema` messages)
// ---------------------------------------------------------------------------

const JSON_TYPES: &[&str] = &[
    "array", "boolean", "integer", "null", "number", "object", "string",
];

/// Why a tool's arguments were not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationFailure {
    /// The tool's own schema is unusable (Python `jsonschema.SchemaError`).
    Schema(String),
    /// The arguments do not match the schema (Python
    /// `jsonschema.ValidationError.message`).
    Instance(String),
}

/// Validate tool arguments against an input schema the way the Python SDK's
/// `jsonschema.validate` does, reporting the most relevant error with
/// jsonschema's wording.
///
/// Supported keywords: `type`, `enum`, `const`, `properties`, `required`,
/// `additionalProperties`, `minProperties`, `maxProperties`,
/// `dependentRequired`, `items`, `prefixItems`, `minItems`, `maxItems`,
/// `uniqueItems`, `minLength`, `maxLength`, `minimum`, `maximum`,
/// `exclusiveMinimum`, `exclusiveMaximum`, `multipleOf`, `allOf`, `anyOf`,
/// `oneOf`, `not`, `if`/`then`/`else`, local `$ref`s and boolean schemas.
/// `pattern`, `patternProperties` and `format` are not checked.
pub fn validate_arguments(instance: &Value, schema: &Value) -> Result<(), ValidationFailure> {
    check_schema(schema).map_err(ValidationFailure::Schema)?;
    let mut errors = Vec::new();
    let validator = Validator {
        root: schema,
        nesting: Cell::new(0),
        overflowed: Cell::new(false),
    };
    validator.validate(instance, schema, 0, &mut errors);
    if validator.overflowed.get() {
        // A `$ref` cycle that never consumes the instance (e.g. `{"$ref":
        // "#"}`) would otherwise recurse until the stack overflows and the
        // process aborts. Python hits RecursionError and reports it as an
        // error result.
        return Err(ValidationFailure::Schema(
            "maximum recursion depth exceeded".to_string(),
        ));
    }
    // jsonschema's best_match: the shallowest error, preferring strong
    // (non anyOf/oneOf) ones.
    let best = errors
        .into_iter()
        .enumerate()
        .min_by_key(|(index, error)| (error.depth, error.weak, *index))
        .map(|(_, error)| error.message);
    match best {
        Some(message) => Err(ValidationFailure::Instance(message)),
        None => Ok(()),
    }
}

fn check_schema(schema: &Value) -> Result<(), String> {
    let Some(object) = schema.as_object() else {
        if schema.is_boolean() {
            return Ok(());
        }
        return Err(format!(
            "{} is not of type 'object', 'boolean'",
            py_repr(schema)
        ));
    };
    if let Some(schema_type) = object.get("type") {
        let valid = match schema_type {
            Value::String(name) => JSON_TYPES.contains(&name.as_str()),
            Value::Array(names) => names
                .iter()
                .all(|name| name.as_str().is_some_and(|name| JSON_TYPES.contains(&name))),
            _ => false,
        };
        if !valid {
            return Err(format!(
                "{} is not valid under any of the given schemas",
                py_repr(schema_type)
            ));
        }
    }
    if let Some(required) = object.get("required") {
        let valid = required
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string));
        if !valid {
            return Err(format!("{} is not of type 'array'", py_repr(required)));
        }
    }
    for keyword in [
        "minimum",
        "maximum",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
        "multipleOf",
    ] {
        if let Some(value) = object.get(keyword) {
            if !value.is_number() {
                return Err(format!("{} is not of type 'number'", py_repr(value)));
            }
        }
    }
    for keyword in ["properties", "$defs", "definitions"] {
        if let Some(value) = object.get(keyword) {
            let Some(map) = value.as_object() else {
                return Err(format!("{} is not of type 'object'", py_repr(value)));
            };
            for sub in map.values() {
                check_schema(sub)?;
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(value) = object.get(keyword) {
            let Some(items) = value.as_array() else {
                return Err(format!("{} is not of type 'array'", py_repr(value)));
            };
            for sub in items {
                check_schema(sub)?;
            }
        }
    }
    for keyword in ["items", "additionalProperties", "not", "if", "then", "else"] {
        if let Some(value) = object.get(keyword) {
            if let Some(items) = value.as_array() {
                for sub in items {
                    check_schema(sub)?;
                }
            } else {
                check_schema(value)?;
            }
        }
    }
    Ok(())
}

struct SchemaError {
    depth: usize,
    weak: bool,
    message: String,
}

/// How deeply `validate` may nest (subschemas, `$ref`s and instance levels
/// together) before validation gives up.
const MAX_VALIDATION_NESTING: usize = 128;

struct Validator<'a> {
    root: &'a Value,
    nesting: Cell<usize>,
    overflowed: Cell<bool>,
}

impl<'a> Validator<'a> {
    fn is_valid(&self, instance: &Value, schema: &Value) -> bool {
        let mut errors = Vec::new();
        self.validate(instance, schema, 0, &mut errors);
        errors.is_empty()
    }

    fn resolve_ref(&self, reference: &str) -> Option<&'a Value> {
        let pointer = reference.strip_prefix('#')?;
        if pointer.is_empty() {
            return Some(self.root);
        }
        self.root.pointer(pointer)
    }

    fn validate(
        &self,
        instance: &Value,
        schema: &Value,
        depth: usize,
        errors: &mut Vec<SchemaError>,
    ) {
        if self.overflowed.get() {
            return;
        }
        if self.nesting.get() >= MAX_VALIDATION_NESTING {
            self.overflowed.set(true);
            return;
        }
        self.nesting.set(self.nesting.get() + 1);
        self.validate_inner(instance, schema, depth, errors);
        self.nesting.set(self.nesting.get() - 1);
    }

    fn validate_inner(
        &self,
        instance: &Value,
        schema: &Value,
        depth: usize,
        errors: &mut Vec<SchemaError>,
    ) {
        let push = |errors: &mut Vec<SchemaError>, message: String| {
            errors.push(SchemaError {
                depth,
                weak: false,
                message,
            });
        };
        let object = match schema {
            Value::Bool(true) => return,
            Value::Bool(false) => {
                push(
                    errors,
                    format!("False schema does not allow {}", py_repr(instance)),
                );
                return;
            }
            Value::Object(object) => object,
            _ => return,
        };

        if let Some(reference) = object.get("$ref").and_then(|v| v.as_str()) {
            if let Some(target) = self.resolve_ref(reference) {
                self.validate(instance, target, depth, errors);
            }
        }

        if let Some(schema_type) = object.get("type") {
            let types: Vec<&str> = match schema_type {
                Value::String(name) => vec![name.as_str()],
                Value::Array(names) => names.iter().filter_map(|n| n.as_str()).collect(),
                _ => Vec::new(),
            };
            if !types.is_empty() && !types.iter().any(|t| is_type(instance, t)) {
                let names = types
                    .iter()
                    .map(|t| py_repr(&Value::String((*t).to_string())))
                    .collect::<Vec<_>>()
                    .join(", ");
                push(
                    errors,
                    format!("{} is not of type {names}", py_repr(instance)),
                );
            }
        }

        if let Some(Value::Array(options)) = object.get("enum") {
            if !options.iter().any(|option| json_equal(option, instance)) {
                push(
                    errors,
                    format!(
                        "{} is not one of {}",
                        py_repr(instance),
                        py_repr(&Value::Array(options.clone()))
                    ),
                );
            }
        }
        if let Some(expected) = object.get("const") {
            if !json_equal(expected, instance) {
                push(errors, format!("{} was expected", py_repr(expected)));
            }
        }

        match instance {
            Value::Object(map) => self.validate_object(map, instance, object, depth, errors),
            Value::Array(items) => self.validate_array(items, instance, object, depth, errors),
            Value::String(text) => {
                let length = text.chars().count() as f64;
                if let Some(min) = number(object, "minLength") {
                    if length < min {
                        let message = if min == 1.0 {
                            "should be non-empty"
                        } else {
                            "is too short"
                        };
                        push(errors, format!("{} {message}", py_repr(instance)));
                    }
                }
                if let Some(max) = number(object, "maxLength") {
                    if length > max {
                        let message = if max == 0.0 {
                            "is expected to be empty"
                        } else {
                            "is too long"
                        };
                        push(errors, format!("{} {message}", py_repr(instance)));
                    }
                }
            }
            Value::Number(value) => {
                let value = value.as_f64().unwrap_or_default();
                let repr = py_repr(instance);
                if let Some(min) = object.get("minimum").filter(|v| v.is_number()) {
                    if value < min.as_f64().unwrap_or_default() {
                        push(
                            errors,
                            format!("{repr} is less than the minimum of {}", py_repr(min)),
                        );
                    }
                }
                if let Some(max) = object.get("maximum").filter(|v| v.is_number()) {
                    if value > max.as_f64().unwrap_or_default() {
                        push(
                            errors,
                            format!("{repr} is greater than the maximum of {}", py_repr(max)),
                        );
                    }
                }
                if let Some(min) = object.get("exclusiveMinimum").filter(|v| v.is_number()) {
                    if value <= min.as_f64().unwrap_or_default() {
                        push(
                            errors,
                            format!(
                                "{repr} is less than or equal to the minimum of {}",
                                py_repr(min)
                            ),
                        );
                    }
                }
                if let Some(max) = object.get("exclusiveMaximum").filter(|v| v.is_number()) {
                    if value >= max.as_f64().unwrap_or_default() {
                        push(
                            errors,
                            format!(
                                "{repr} is greater than or equal to the maximum of {}",
                                py_repr(max)
                            ),
                        );
                    }
                }
                if let Some(divisor) = object.get("multipleOf").filter(|v| v.is_number()) {
                    let d = divisor.as_f64().unwrap_or_default();
                    if d != 0.0 {
                        let quotient = value / d;
                        if (quotient - quotient.round()).abs() > 1e-9 {
                            push(
                                errors,
                                format!("{repr} is not a multiple of {}", py_repr(divisor)),
                            );
                        }
                    }
                }
            }
            _ => {}
        }

        if let Some(Value::Array(subschemas)) = object.get("allOf") {
            for sub in subschemas {
                self.validate(instance, sub, depth, errors);
            }
        }
        if let Some(Value::Array(subschemas)) = object.get("anyOf") {
            if !subschemas.iter().any(|sub| self.is_valid(instance, sub)) {
                errors.push(SchemaError {
                    depth,
                    weak: true,
                    message: format!(
                        "{} is not valid under any of the given schemas",
                        py_repr(instance)
                    ),
                });
            }
        }
        if let Some(Value::Array(subschemas)) = object.get("oneOf") {
            let matching: Vec<&Value> = subschemas
                .iter()
                .filter(|sub| self.is_valid(instance, sub))
                .collect();
            if matching.is_empty() {
                errors.push(SchemaError {
                    depth,
                    weak: true,
                    message: format!(
                        "{} is not valid under any of the given schemas",
                        py_repr(instance)
                    ),
                });
            } else if matching.len() > 1 {
                let reprs = matching
                    .iter()
                    .take(2)
                    .map(|sub| py_repr(sub))
                    .collect::<Vec<_>>()
                    .join(", ");
                errors.push(SchemaError {
                    depth,
                    weak: true,
                    message: format!("{} is valid under each of {reprs}", py_repr(instance)),
                });
            }
        }
        if let Some(not_schema) = object.get("not") {
            if self.is_valid(instance, not_schema) {
                push(
                    errors,
                    format!(
                        "{} should not be valid under {}",
                        py_repr(instance),
                        py_repr(not_schema)
                    ),
                );
            }
        }
        if let Some(condition) = object.get("if") {
            let branch = if self.is_valid(instance, condition) {
                object.get("then")
            } else {
                object.get("else")
            };
            if let Some(branch) = branch {
                self.validate(instance, branch, depth, errors);
            }
        }
    }

    fn validate_object(
        &self,
        map: &Map<String, Value>,
        instance: &Value,
        object: &Map<String, Value>,
        depth: usize,
        errors: &mut Vec<SchemaError>,
    ) {
        let push = |errors: &mut Vec<SchemaError>, message: String| {
            errors.push(SchemaError {
                depth,
                weak: false,
                message,
            });
        };
        let properties = object.get("properties").and_then(|v| v.as_object());
        if let Some(properties) = properties {
            for (name, sub) in properties {
                if let Some(value) = map.get(name) {
                    self.validate(value, sub, depth + 1, errors);
                }
            }
        }
        if let Some(Value::Array(required)) = object.get("required") {
            for name in required.iter().filter_map(|v| v.as_str()) {
                if !map.contains_key(name) {
                    push(
                        errors,
                        format!(
                            "{} is a required property",
                            py_repr(&Value::String(name.to_string()))
                        ),
                    );
                }
            }
        }
        if let Some(additional) = object.get("additionalProperties") {
            // Without regex support, patternProperties would make every
            // matching key look "additional"; skip the check then.
            if !object.contains_key("patternProperties") {
                let mut extras: Vec<&String> = map
                    .keys()
                    .filter(|key| properties.is_none_or(|p| !p.contains_key(*key)))
                    .collect();
                extras.sort();
                match additional {
                    Value::Bool(false) if !extras.is_empty() => {
                        let verb = if extras.len() == 1 { "was" } else { "were" };
                        let listed = extras
                            .iter()
                            .map(|key| py_repr(&Value::String((*key).clone())))
                            .collect::<Vec<_>>()
                            .join(", ");
                        push(
                            errors,
                            format!(
                                "Additional properties are not allowed ({listed} {verb} unexpected)"
                            ),
                        );
                    }
                    Value::Object(_) => {
                        for key in extras {
                            if let Some(value) = map.get(key) {
                                self.validate(value, additional, depth + 1, errors);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let count = map.len() as f64;
        if let Some(min) = number(object, "minProperties") {
            if count < min {
                let message = if min == 1.0 {
                    "should be non-empty"
                } else {
                    "does not have enough properties"
                };
                push(errors, format!("{} {message}", py_repr(instance)));
            }
        }
        if let Some(max) = number(object, "maxProperties") {
            if count > max {
                let message = if max == 0.0 {
                    "is expected to be empty"
                } else {
                    "has too many properties"
                };
                push(errors, format!("{} {message}", py_repr(instance)));
            }
        }
        if let Some(Value::Object(dependent)) = object.get("dependentRequired") {
            for (property, needed) in dependent {
                if !map.contains_key(property) {
                    continue;
                }
                for each in needed.as_array().into_iter().flatten() {
                    if let Some(each) = each.as_str() {
                        if !map.contains_key(each) {
                            push(
                                errors,
                                format!(
                                    "{} is a dependency of {}",
                                    py_repr(&Value::String(each.to_string())),
                                    py_repr(&Value::String(property.clone()))
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    fn validate_array(
        &self,
        items: &[Value],
        instance: &Value,
        object: &Map<String, Value>,
        depth: usize,
        errors: &mut Vec<SchemaError>,
    ) {
        let push = |errors: &mut Vec<SchemaError>, message: String| {
            errors.push(SchemaError {
                depth,
                weak: false,
                message,
            });
        };
        let mut prefix = 0;
        if let Some(Value::Array(prefix_items)) = object.get("prefixItems") {
            prefix = prefix_items.len();
            for (item, sub) in items.iter().zip(prefix_items) {
                self.validate(item, sub, depth + 1, errors);
            }
        }
        match object.get("items") {
            // Draft 4-7 tuple form.
            Some(Value::Array(tuple)) => {
                for (item, sub) in items.iter().zip(tuple) {
                    self.validate(item, sub, depth + 1, errors);
                }
            }
            Some(sub) => {
                for item in items.iter().skip(prefix) {
                    self.validate(item, sub, depth + 1, errors);
                }
            }
            None => {}
        }
        let count = items.len() as f64;
        if let Some(min) = number(object, "minItems") {
            if count < min {
                let message = if min == 1.0 {
                    "should be non-empty"
                } else {
                    "is too short"
                };
                push(errors, format!("{} {message}", py_repr(instance)));
            }
        }
        if let Some(max) = number(object, "maxItems") {
            if count > max {
                let message = if max == 0.0 {
                    "is expected to be empty"
                } else {
                    "is too long"
                };
                push(errors, format!("{} {message}", py_repr(instance)));
            }
        }
        if object.get("uniqueItems") == Some(&Value::Bool(true)) {
            let duplicate = items
                .iter()
                .enumerate()
                .any(|(i, a)| items[i + 1..].iter().any(|b| json_equal(a, b)));
            if duplicate {
                push(
                    errors,
                    format!("{} has non-unique elements", py_repr(instance)),
                );
            }
        }
    }
}

fn number(object: &Map<String, Value>, keyword: &str) -> Option<f64> {
    object.get(keyword).and_then(|v| v.as_f64())
}

fn is_type(instance: &Value, name: &str) -> bool {
    match name {
        "null" => instance.is_null(),
        "boolean" => instance.is_boolean(),
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "number" => instance.is_number(),
        "integer" => match instance {
            Value::Number(n) => {
                n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
            }
            _ => false,
        },
        _ => false,
    }
}

/// JSON equality as jsonschema sees it: `1 == 1.0`, but `true != 1`.
fn json_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_equal(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, value)| y.get(key).is_some_and(|other| json_equal(value, other)))
        }
        _ => a == b,
    }
}

/// Python `repr()` of a JSON value, as used in jsonschema messages.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => py_repr_str(text),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(key, value)| format!("{}: {}", py_repr_str(key), py_repr(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn py_repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_repr_matches_python() {
        assert_eq!(py_repr(&json!("two")), "'two'");
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(py_repr(&json!(1)), "1");
        assert_eq!(py_repr(&json!(1.5)), "1.5");
        assert_eq!(py_repr(&json!(null)), "None");
        assert_eq!(py_repr(&json!([true, false])), "[True, False]");
        assert_eq!(py_repr(&json!({"a": "b"})), "{'a': 'b'}");
    }

    #[test]
    fn validation_messages_match_jsonschema() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
            "required": ["a", "b"],
        });
        assert_eq!(
            validate_arguments(&json!({"a": 1}), &schema),
            Err(ValidationFailure::Instance(
                "'b' is a required property".to_string()
            ))
        );
        assert_eq!(
            validate_arguments(&json!({"a": 1, "b": "two"}), &schema),
            Err(ValidationFailure::Instance(
                "'two' is not of type 'number'".to_string()
            ))
        );
        assert_eq!(
            validate_arguments(&json!({"a": 1, "b": 2.5}), &schema),
            Ok(())
        );

        let strict = json!({"type": "object", "properties": {}, "additionalProperties": false});
        assert_eq!(
            validate_arguments(&json!({"x": 1, "y": 2}), &strict),
            Err(ValidationFailure::Instance(
                "Additional properties are not allowed ('x', 'y' were unexpected)".to_string()
            ))
        );

        let short =
            json!({"type": "object", "properties": {"name": {"type": "string", "minLength": 2}}});
        assert_eq!(
            validate_arguments(&json!({"name": "x"}), &short),
            Err(ValidationFailure::Instance("'x' is too short".to_string()))
        );

        let integer = json!({"type": "integer"});
        assert_eq!(validate_arguments(&json!(3.0), &integer), Ok(()));
        assert!(validate_arguments(&json!(true), &json!({"type": "number"})).is_err());
    }

    #[test]
    fn ref_cycle_is_a_schema_failure_not_a_stack_overflow() {
        for schema in [
            json!({"type": "object", "properties": {}, "$ref": "#"}),
            json!({"$defs": {"a": {"$ref": "#/$defs/b"}, "b": {"$ref": "#/$defs/a"}}, "$ref": "#/$defs/a"}),
            json!({"type": "object", "properties": {}, "anyOf": [{"$ref": "#"}]}),
        ] {
            assert_eq!(
                validate_arguments(&json!({}), &schema),
                Err(ValidationFailure::Schema(
                    "maximum recursion depth exceeded".to_string()
                ))
            );
        }
    }

    #[test]
    fn recursive_ref_over_a_finite_instance_still_validates() {
        let schema = json!({
            "$defs": {"node": {
                "type": "object",
                "properties": {"child": {"$ref": "#/$defs/node"}, "v": {"type": "integer"}},
            }},
            "$ref": "#/$defs/node",
        });
        let mut tree = json!({"v": 1});
        for _ in 0..20 {
            tree = json!({"v": 1, "child": tree});
        }
        assert_eq!(validate_arguments(&tree, &schema), Ok(()));
        tree["child"]["child"]["v"] = json!("x");
        assert_eq!(
            validate_arguments(&tree, &schema),
            Err(ValidationFailure::Instance(
                "'x' is not of type 'integer'".to_string()
            ))
        );
    }

    #[test]
    fn invalid_schema_is_a_schema_failure() {
        let broken = json!({"type": "object", "properties": {"x": {"type": "bogus"}}});
        match validate_arguments(&json!({"x": 1}), &broken) {
            Err(ValidationFailure::Schema(message)) => assert!(message.contains("bogus")),
            other => panic!("unexpected {other:?}"),
        }
    }
}
