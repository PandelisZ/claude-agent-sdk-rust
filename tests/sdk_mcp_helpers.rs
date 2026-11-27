use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use claude_code_sdk_rust::internal::sdk_mcp::{
    answer_mcp_control_request, answer_mcp_message, build_input_schema,
};
use claude_code_sdk_rust::mcp::{
    async_tool, async_tool_with_annotations, SdkMcpServer, SdkMcpToolResult,
};
use claude_code_sdk_rust::{
    create_sdk_mcp_server, create_sdk_mcp_server_with_version, tool, tool_with_annotations,
    MCPContent, MCPToolAnnotations, SdkMcpTool, SimpleMCPServer,
};
use serde_json::{json, Value};

const INITIALIZE_PARAMS: &str = r#"{
    "protocolVersion": "2025-06-18",
    "capabilities": {},
    "clientInfo": {"name": "test-client", "version": "0.0.0"}
}"#;

fn servers(server: SimpleMCPServer) -> HashMap<String, SimpleMCPServer> {
    HashMap::from([("srv".to_string(), server)])
}

async fn request(
    servers: &HashMap<String, SimpleMCPServer>,
    id: i64,
    method: &str,
    params: Value,
) -> Value {
    let response = answer_mcp_message(
        servers,
        "srv",
        &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
    )
    .await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], id);
    response
}

async fn call(servers: &HashMap<String, SimpleMCPServer>, name: &str, arguments: Value) -> Value {
    let response = request(
        servers,
        1,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await;
    assert!(response.get("error").is_none(), "{response}");
    response["result"].clone()
}

async fn list_tools(servers: &HashMap<String, SimpleMCPServer>) -> Vec<Value> {
    request(servers, 1, "tools/list", json!({})).await["result"]["tools"]
        .as_array()
        .unwrap()
        .clone()
}

fn texts(result: &Value) -> Vec<String> {
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "text")
        .map(|block| block["text"].as_str().unwrap().to_string())
        .collect()
}

// --- Tool helpers -------------------------------------------------------------

#[tokio::test]
async fn sdk_mcp_tool_helper_registers_and_calls_tool() {
    let greet = tool(
        "greet",
        "Greet a user",
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        }),
        |input| {
            let name = input
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("there");
            Ok(vec![MCPContent::Text {
                text: format!("Hello, {name}!"),
            }])
        },
    );
    let server = create_sdk_mcp_server("people", vec![greet]);

    let tools = server.list_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "greet");
    assert_eq!(tools[0].description, "Greet a user");

    let result = server.call_tool("greet", json!({"name": "Ada"})).await;
    assert_eq!(result, SdkMcpToolResult::text("Hello, Ada!"));
}

#[tokio::test]
async fn async_tool_handler_is_awaited_and_returns_content_with_is_error() {
    let divide = async_tool(
        "divide",
        "Divide two numbers",
        json!({"a": "number", "b": "number"}),
        |args| async move {
            tokio::task::yield_now().await;
            let a = args["a"].as_f64().unwrap();
            let b = args["b"].as_f64().unwrap();
            if b == 0.0 {
                return SdkMcpToolResult::error("Division by zero");
            }
            SdkMcpToolResult::text(format!("{}", a / b))
        },
    );
    let direct = divide.call(json!({"a": 1, "b": 0})).await;
    assert!(direct.is_error);

    let servers = servers(create_sdk_mcp_server("srv", vec![divide]));
    let failure = call(&servers, "divide", json!({"a": 1, "b": 0})).await;
    let success = call(&servers, "divide", json!({"a": 4, "b": 2})).await;

    assert_eq!(failure["isError"], true);
    assert!(failure.get("is_error").is_none());
    assert_eq!(texts(&failure), ["Division by zero"]);
    assert_eq!(
        success,
        json!({"content": [{"type": "text", "text": "2"}], "isError": false})
    );
}

#[tokio::test]
async fn async_tool_err_and_panic_are_error_results() {
    let fails = async_tool("fails", "Fails", json!({}), |_| async {
        Err::<SdkMcpToolResult, _>("Expected error")
    });
    let panics = async_tool("panics", "Panics", json!({}), |_| async {
        if true {
            panic!("boom");
        }
        SdkMcpToolResult::default()
    });
    let sync_fails = tool("sync_fails", "Fails", json!({}), |_| {
        Err("Sync error".to_string())
    });
    let servers = servers(create_sdk_mcp_server(
        "srv",
        vec![fails, panics, sync_fails],
    ));

    let result = call(&servers, "fails", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(texts(&result), ["Expected error"]);

    let result = call(&servers, "panics", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(texts(&result), ["boom"]);

    let result = call(&servers, "sync_fails", json!({})).await;
    assert_eq!(
        result,
        json!({"content": [{"type": "text", "text": "Sync error"}], "isError": true})
    );
}

#[tokio::test]
async fn unknown_tool_is_an_error_result() {
    let servers = servers(create_sdk_mcp_server("srv", Vec::new()));
    let result = call(&servers, "mystery", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(texts(&result), ["Tool 'mystery' not found"]);
}

#[tokio::test]
async fn register_async_tool_on_simple_server() {
    let mut server = SimpleMCPServer::new("srv");
    server.register_async_tool(
        claude_code_sdk_rust::MCPTool {
            name: "echo".to_string(),
            description: "Echo".to_string(),
            input_schema: json!({"text": "string"}),
            annotations: None,
        },
        |args| async move { vec![MCPContent::text(args["text"].as_str().unwrap_or(""))] },
    );
    let servers = servers(server);
    assert_eq!(
        texts(&call(&servers, "echo", json!({"text": "hi"})).await),
        ["hi"]
    );
}

// --- Handshake and JSON-RPC ---------------------------------------------------

#[tokio::test]
async fn initialize_reports_server_info_capabilities_and_negotiates_version() {
    let servers = servers(create_sdk_mcp_server_with_version(
        "hello",
        "3.2.1",
        Vec::new(),
    ));
    let params: Value = serde_json::from_str(INITIALIZE_PARAMS).unwrap();

    let response = request(&servers, 0, "initialize", params).await;
    assert_eq!(
        response["result"],
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"experimental": {}, "tools": {"listChanged": false}},
            "serverInfo": {"name": "hello", "version": "3.2.1"},
        })
    );

    let response = request(
        &servers,
        1,
        "initialize",
        json!({"protocolVersion": "1999-01-01", "capabilities": {}}),
    )
    .await;
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
}

#[tokio::test]
async fn ping_is_answered_with_an_empty_result() {
    let servers = servers(create_sdk_mcp_server("srv", Vec::new()));
    let response = request(&servers, 3, "ping", json!({})).await;
    assert_eq!(response, json!({"jsonrpc": "2.0", "id": 3, "result": {}}));
}

#[tokio::test]
async fn every_notification_is_acknowledged_without_a_jsonrpc_reply() {
    let server = create_sdk_mcp_server("srv", Vec::new());
    for method in [
        "notifications/initialized",
        "notifications/cancelled",
        "notifications/roots/list_changed",
    ] {
        let message = json!({"jsonrpc": "2.0", "method": method});
        assert_eq!(server.handle_message(&message).await, None);
    }
    // A JSON-RPC response from the client gets no reply either.
    let response = json!({"jsonrpc": "2.0", "id": 4, "result": {}});
    assert_eq!(server.handle_message(&response).await, None);
    // A frame whose id is not a valid JSON-RPC id reads as a notification.
    let odd = json!({"jsonrpc": "2.0", "id": 2.5, "method": "tools/list"});
    assert_eq!(server.handle_message(&odd).await, None);

    let servers = servers(server);
    let ack = answer_mcp_message(
        &servers,
        "srv",
        &json!({"jsonrpc": "2.0", "method": "notifications/progress"}),
    )
    .await;
    assert_eq!(ack, json!({"jsonrpc": "2.0", "result": {}}));
}

#[tokio::test]
async fn control_request_for_a_notification_is_acknowledged() {
    let servers = servers(create_sdk_mcp_server("srv", Vec::new()));
    let request = json!({
        "subtype": "mcp_message",
        "server_name": "srv",
        "message": {"jsonrpc": "2.0", "method": "notifications/initialized"},
    });
    let response = answer_mcp_control_request(&servers, request.as_object().unwrap())
        .await
        .unwrap();
    assert_eq!(
        response,
        json!({"mcp_response": {"jsonrpc": "2.0", "result": {}}})
    );
}

#[tokio::test]
async fn control_request_without_server_name_or_message_is_a_control_error() {
    let servers = servers(create_sdk_mcp_server("srv", Vec::new()));
    for request in [
        json!({"subtype": "mcp_message", "message": {"jsonrpc": "2.0", "id": 1, "method": "ping"}}),
        json!({"subtype": "mcp_message", "server_name": "srv"}),
        json!({"subtype": "mcp_message", "server_name": "", "message": {"id": 1}}),
        json!({"subtype": "mcp_message", "server_name": "srv", "message": {}}),
    ] {
        let error = answer_mcp_control_request(&servers, request.as_object().unwrap())
            .await
            .unwrap_err();
        assert_eq!(error, "Missing server_name or message for MCP request");
    }
}

#[tokio::test]
async fn unknown_server_and_method_are_jsonrpc_errors() {
    let servers = servers(create_sdk_mcp_server("srv", Vec::new()));
    let response = answer_mcp_message(
        &servers,
        "missing",
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(
        response,
        json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "Server 'missing' not found"}})
    );

    let response = request(&servers, 2, "resources/list", json!({})).await;
    assert_eq!(
        response["error"],
        json!({"code": -32601, "message": "Method not found"})
    );
}

#[tokio::test]
async fn malformed_message_is_an_internal_error_and_the_server_survives() {
    let servers = servers(create_sdk_mcp_server("srv", Vec::new()));
    let response = answer_mcp_message(&servers, "srv", &json!({"jsonrpc": "2.0", "id": 5})).await;
    assert_eq!(response["id"], 5);
    assert_eq!(response["error"]["code"], -32603);
    assert!(list_tools(&servers).await.is_empty());
}

// --- tools/list wire format ---------------------------------------------------

#[tokio::test]
async fn tools_list_emits_only_set_hints_and_keeps_registration_order() {
    let read_data = tool_with_annotations(
        "read_data",
        "Read data from source",
        json!({"source": "string"}),
        MCPToolAnnotations::new()
            .read_only_hint(true)
            .open_world_hint(false),
        |_| Ok(Vec::new()),
    );
    let delete_item = tool_with_annotations(
        "delete_item",
        "Delete an item",
        json!({"id": "string"}),
        MCPToolAnnotations {
            destructive_hint: Some(true),
            idempotent_hint: Some(true),
            ..Default::default()
        },
        |_| Ok(Vec::new()),
    );
    let plain = tool(
        "plain",
        "Tool without annotations",
        json!({"x": "string", "n": "integer"}),
        |_| Ok(Vec::new()),
    );
    let servers = servers(create_sdk_mcp_server(
        "srv",
        vec![read_data, delete_item, plain],
    ));
    let tools = list_tools(&servers).await;

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["read_data", "delete_item", "plain"]);
    assert_eq!(
        tools[2],
        json!({
            "name": "plain",
            "description": "Tool without annotations",
            "inputSchema": {
                "type": "object",
                "properties": {"x": {"type": "string"}, "n": {"type": "integer"}},
                "required": ["x", "n"],
            },
        })
    );
    assert_eq!(
        tools[0]["annotations"],
        json!({"readOnlyHint": true, "openWorldHint": false})
    );
    assert_eq!(
        tools[1]["annotations"],
        json!({"destructiveHint": true, "idempotentHint": true})
    );
    assert!(tools[0].get("_meta").is_none());
    assert!(tools[2].get("_meta").is_none());
}

#[tokio::test]
async fn max_result_size_chars_travels_in_meta_not_in_annotations() {
    let inspect = tool_with_annotations(
        "inspect",
        "Inspect project state",
        json!({"type": "object"}),
        MCPToolAnnotations {
            title: Some("Inspect".to_string()),
            read_only_hint: Some(true),
            max_result_size_chars: Some(4096),
            ..Default::default()
        },
        |_| Ok(vec![MCPContent::text("ok")]),
    );
    let hand_built = tool_with_annotations(
        "hand_built",
        "Only a size",
        json!({}),
        MCPToolAnnotations::new().max_result_size_chars(333),
        |_| Ok(Vec::new()),
    );
    let servers = servers(create_sdk_mcp_server("people", vec![inspect, hand_built]));
    let tools = list_tools(&servers).await;

    assert_eq!(
        tools[0]["annotations"],
        json!({"title": "Inspect", "readOnlyHint": true})
    );
    assert_eq!(
        tools[0]["_meta"],
        json!({"anthropic/maxResultSizeChars": 4096})
    );
    assert_eq!(tools[1]["annotations"], json!({}));
    assert_eq!(
        tools[1]["_meta"],
        json!({"anthropic/maxResultSizeChars": 333})
    );
}

#[test]
fn tool_annotations_take_either_spelling() {
    let camel: MCPToolAnnotations = serde_json::from_value(json!({
        "readOnlyHint": true, "destructiveHint": false, "maxResultSizeChars": 11
    }))
    .unwrap();
    let snake: MCPToolAnnotations = serde_json::from_value(json!({
        "read_only_hint": true, "destructive_hint": false, "max_result_size_chars": 11
    }))
    .unwrap();
    assert_eq!(camel, snake);
    // Like `ToolAnnotations.model_dump(by_alias=True, exclude_none=True)`.
    assert_eq!(
        serde_json::to_value(&camel).unwrap(),
        json!({"readOnlyHint": true, "destructiveHint": false, "maxResultSizeChars": 11})
    );
}

#[test]
fn input_schemas_are_normalized_like_python() {
    // A full JSON Schema is sent verbatim.
    let full =
        json!({"type": "object", "properties": {"a": {"type": "number"}}, "required": ["a"]});
    assert_eq!(build_input_schema(&full), full);
    // A {param: type} map becomes an object schema with every key required.
    assert_eq!(
        build_input_schema(
            &json!({"a": "number", "tags": {"type": "array", "items": {"type": "string"}}})
        ),
        json!({
            "type": "object",
            "properties": {"a": {"type": "number"}, "tags": {"type": "array", "items": {"type": "string"}}},
            "required": ["a", "tags"],
        })
    );
    assert_eq!(
        build_input_schema(&json!({})),
        json!({"type": "object", "properties": {}, "required": []})
    );
    // A non-object schema lists as an empty object.
    assert_eq!(
        build_input_schema(&Value::Null),
        json!({"type": "object", "properties": {}})
    );
    // `{"type": "object"}` stays a JSON Schema (gaining empty properties).
    assert_eq!(
        build_input_schema(&json!({"type": "object", "additionalProperties": false})),
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    );
}

// --- tools/call validation ----------------------------------------------------

#[tokio::test]
async fn invalid_arguments_are_rejected_before_the_handler_runs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let add = tool(
        "add",
        "Add two numbers",
        json!({"a": "number", "b": "number"}),
        move |args| {
            counter.fetch_add(1, Ordering::SeqCst);
            let sum = args["a"].as_f64().unwrap() + args["b"].as_f64().unwrap();
            Ok(vec![MCPContent::text(format!("{sum}"))])
        },
    );
    let servers = servers(create_sdk_mcp_server("srv", vec![add]));

    let missing = call(&servers, "add", json!({"a": 1})).await;
    let wrong_type = call(&servers, "add", json!({"a": 1, "b": "two"})).await;
    let fine = call(&servers, "add", json!({"a": 1, "b": 2})).await;

    assert_eq!(missing["isError"], true);
    assert_eq!(
        texts(&missing),
        ["Input validation error: 'b' is a required property"]
    );
    assert_eq!(wrong_type["isError"], true);
    let message = &texts(&wrong_type)[0];
    assert!(message.starts_with("Input validation error: "));
    assert!(message.contains("'two'"));
    assert_eq!(texts(&fine), ["3"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn json_schema_constraints_are_enforced() {
    let validate = tool(
        "validate",
        "Validate input",
        json!({
            "type": "object",
            "properties": {"name": {"type": "string", "minLength": 2}},
            "required": ["name"],
        }),
        |_| Ok(vec![MCPContent::text("OK")]),
    );
    let servers = servers(create_sdk_mcp_server("srv", vec![validate]));
    let too_short = call(&servers, "validate", json!({"name": "x"})).await;
    let ok = call(&servers, "validate", json!({"name": "xy"})).await;
    assert_eq!(too_short["isError"], true);
    assert!(texts(&too_short)[0].starts_with("Input validation error: "));
    assert_eq!(texts(&ok), ["OK"]);
}

#[tokio::test]
async fn invalid_tool_schema_is_an_error_result() {
    let broken = SdkMcpTool::new(
        "broken",
        "Has an invalid schema",
        json!({"type": "object", "properties": {"x": {"type": "bogus"}}}),
        None,
        |_| Ok(vec![MCPContent::text("unreachable")]),
    );
    let servers = servers(create_sdk_mcp_server("srv", vec![broken]));
    let result = call(&servers, "broken", json!({"x": 1})).await;
    assert_eq!(result["isError"], true);
    assert!(texts(&result)[0].contains("bogus"));
}

#[tokio::test]
async fn missing_arguments_default_to_an_empty_object() {
    let noop = tool("noop", "No args", json!({}), |args| {
        Ok(vec![MCPContent::text(args.to_string())])
    });
    let servers = servers(create_sdk_mcp_server("srv", vec![noop]));
    let response = request(&servers, 1, "tools/call", json!({"name": "noop"})).await;
    assert_eq!(texts(&response["result"]), ["{}"]);
}

// --- Content conversion -------------------------------------------------------

#[tokio::test]
async fn content_is_converted_like_python() {
    let mixed = tool("mixed", "Mixed content", json!({}), |_| {
        Ok(vec![
            MCPContent::text("Here is the document:"),
            MCPContent::image("iVBORw0KGgo=", "image/png"),
            MCPContent::ResourceLink {
                uri: "https://example.com/doc.pdf".to_string(),
                name: Some("My Document".to_string()),
                description: Some("A test document".to_string()),
                mime_type: None,
            },
            MCPContent::ResourceLink {
                uri: "https://example.com/report".to_string(),
                name: Some("Report".to_string()),
                description: None,
                mime_type: Some("text/html".to_string()),
            },
            MCPContent::ResourceLink {
                uri: String::new(),
                name: None,
                description: None,
                mime_type: None,
            },
            MCPContent::Resource {
                uri: "file:///test.txt".to_string(),
                mime_type: Some("text/plain".to_string()),
                text: Some("File contents here".to_string()),
            },
            // Binary resources and audio are dropped.
            MCPContent::Resource {
                uri: "file:///image.png".to_string(),
                mime_type: Some("image/png".to_string()),
                text: None,
            },
            MCPContent::Audio {
                data: "abc123".to_string(),
                mime_type: "audio/wav".to_string(),
            },
        ])
    });
    let servers = servers(create_sdk_mcp_server("srv", vec![mixed]));
    let result = call(&servers, "mixed", json!({})).await;
    assert_eq!(
        result,
        json!({
            "content": [
                {"type": "text", "text": "Here is the document:"},
                {"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"},
                {"type": "text", "text": "My Document\nhttps://example.com/doc.pdf\nA test document"},
                {"type": "text", "text": "Report\nhttps://example.com/report"},
                {"type": "text", "text": "Resource link"},
                {"type": "text", "text": "File contents here"},
            ],
            "isError": false,
        })
    );
}

#[test]
fn mcp_content_serde_uses_python_wire_names() {
    let image = MCPContent::image("abc", "image/png");
    assert_eq!(
        serde_json::to_value(&image).unwrap(),
        json!({"type": "image", "data": "abc", "mimeType": "image/png"})
    );
    let parsed: SdkMcpToolResult = serde_json::from_value(json!({
        "content": [{"type": "text", "text": "hi"}],
        "is_error": true
    }))
    .unwrap();
    assert_eq!(parsed, SdkMcpToolResult::error("hi"));
}

// --- Concurrency, cancellation, in-flight ids --------------------------------

#[tokio::test]
async fn cancelled_tool_call_is_ended_with_request_cancelled() {
    let started = Arc::new(tokio::sync::Notify::new());
    let finished = Arc::new(AtomicUsize::new(0));
    let (started_tx, finished_tx) = (Arc::clone(&started), Arc::clone(&finished));
    let slow = async_tool("slow", "Sleeps", json!({}), move |_| {
        let started = Arc::clone(&started_tx);
        let finished = Arc::clone(&finished_tx);
        async move {
            started.notify_one();
            tokio::time::sleep(Duration::from_secs(30)).await;
            finished.fetch_add(1, Ordering::SeqCst);
            SdkMcpToolResult::default()
        }
    });
    let server = create_sdk_mcp_server("srv", vec![slow]);
    let call_message = json!({
        "jsonrpc": "2.0", "id": 77, "method": "tools/call",
        "params": {"name": "slow", "arguments": {}}
    });
    let cancel = json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": 77, "reason": "user interrupted"}
    });

    let (response, ack) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server.handle_message(&call_message), async {
            started.notified().await;
            server.handle_message(&cancel).await
        })
    })
    .await
    .expect("cancellation ends the call");

    assert_eq!(ack, None);
    assert_eq!(
        response.unwrap(),
        json!({"jsonrpc": "2.0", "id": 77, "error": {"code": -32800, "message": "Request cancelled"}})
    );
    assert_eq!(finished.load(Ordering::SeqCst), 0);
    // The server carries on.
    let servers = servers(server);
    assert_eq!(list_tools(&servers).await.len(), 1);
}

#[tokio::test]
async fn reusing_an_in_flight_request_id_is_refused() {
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (release_rx, counter) = (Arc::clone(&release), Arc::clone(&calls));
    let wait = async_tool("wait", "Waits", json!({}), move |_| {
        let release = Arc::clone(&release_rx);
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            release.notified().await;
            SdkMcpToolResult::text("done")
        }
    });
    let server = create_sdk_mcp_server("srv", vec![wait]);
    let message = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": "wait", "arguments": {}}
    });

    let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server.handle_message(&message), async {
            tokio::task::yield_now().await;
            let second = server.handle_message(&message).await;
            release.notify_one();
            second
        })
    })
    .await
    .unwrap();

    let second = second.unwrap();
    assert_eq!(second["id"], 7);
    assert_eq!(second["error"]["code"], -32603);
    assert_eq!(
        second["error"]["message"],
        "Request id 7 is already in flight"
    );
    assert_eq!(texts(&first.unwrap()["result"]), ["done"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Once answered, the id can be used again.
    release.notify_one();
    let again = server.handle_message(&message).await.unwrap();
    assert_eq!(texts(&again["result"]), ["done"]);
}

#[tokio::test]
async fn concurrent_tool_calls_on_one_server_both_resolve() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let shared = Arc::clone(&barrier);
    let echo = async_tool("echo", "Echo", json!({"text": "string"}), move |args| {
        let barrier = Arc::clone(&shared);
        async move {
            // Both calls must be running at once to get past this.
            barrier.wait().await;
            SdkMcpToolResult::text(args["text"].as_str().unwrap_or_default())
        }
    });
    let server = create_sdk_mcp_server("srv", vec![echo]);
    let message = |id: i64, text: &str| {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
               "params": {"name": "echo", "arguments": {"text": text}}})
    };
    let (a, b) = (message(1, "a"), message(2, "b"));
    let (ra, rb) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server.handle_message(&a), server.handle_message(&b))
    })
    .await
    .unwrap();
    assert_eq!(texts(&ra.unwrap()["result"]), ["a"]);
    assert_eq!(texts(&rb.unwrap()["result"]), ["b"]);
}

// --- User-supplied servers ----------------------------------------------------

struct HandBuilt;

#[async_trait::async_trait]
impl SdkMcpServer for HandBuilt {
    async fn handle_message(&self, message: Value) -> Result<Option<Value>, String> {
        let Some(id) = message.get("id").cloned() else {
            return Ok(None);
        };
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => Ok(Some(json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}, "resources": {}},
                "serverInfo": {"name": "hand-built", "version": "0.1.0"},
            }}))),
            "resources/list" => Ok(Some(json!({"jsonrpc": "2.0", "id": id, "result": {
                "resources": [{"uri": "memo://readme", "name": "readme"}]
            }}))),
            "silent" => Ok(None),
            _ => Err("cannot serve today".to_string()),
        }
    }
}

#[tokio::test]
async fn hand_built_server_is_served_verbatim() {
    let server = SimpleMCPServer::from_server("hand-built", Arc::new(HandBuilt));
    assert!(server.is_custom());
    let servers = servers(server);

    let params: Value = serde_json::from_str(INITIALIZE_PARAMS).unwrap();
    let info = request(&servers, 0, "initialize", params).await;
    assert_eq!(
        info["result"]["serverInfo"],
        json!({"name": "hand-built", "version": "0.1.0"})
    );
    let resources = request(&servers, 1, "resources/list", json!({})).await;
    assert_eq!(resources["result"]["resources"][0]["uri"], "memo://readme");

    let failed = request(&servers, 2, "tools/list", json!({})).await;
    assert_eq!(
        failed["error"],
        json!({"code": -32603, "message": "cannot serve today"})
    );
    let unanswered = request(&servers, 3, "silent", json!({})).await;
    assert_eq!(unanswered["error"]["code"], -32800);

    let ack = answer_mcp_message(
        &servers,
        "srv",
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(ack, json!({"jsonrpc": "2.0", "result": {}}));
}

#[tokio::test]
async fn async_tool_with_annotations_lists_its_hints() {
    let search = async_tool_with_annotations(
        "search",
        "Search the web",
        json!({"query": "string"}),
        MCPToolAnnotations::new().open_world_hint(true),
        |_| async { Vec::<MCPContent>::new() },
    );
    let servers = servers(create_sdk_mcp_server("srv", vec![search]));
    let tools = list_tools(&servers).await;
    assert_eq!(tools[0]["annotations"], json!({"openWorldHint": true}));
}
