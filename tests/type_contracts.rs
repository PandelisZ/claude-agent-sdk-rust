use claude_code_sdk_rust::{
    AgentDefinition, ClaudeAgentOptions, ContentBlock, MCPServerConnectionStatus, MCPServerStatus,
    MCPServerStatusConfig, MCPStatusResponse, PermissionMode, PermissionRuleValue,
    PermissionUpdate, SettingSource,
};

#[test]
fn permission_update_round_trips_python_wire_shapes() {
    let add_rules = PermissionUpdate {
        r#type: "addRules".to_string(),
        destination: Some("localSettings".to_string()),
        behavior: Some("allow".to_string()),
        rules: Some(vec![
            PermissionRuleValue {
                tool_name: "Bash".to_string(),
                rule_content: Some("npm *".to_string()),
            },
            PermissionRuleValue {
                tool_name: "Read".to_string(),
                rule_content: None,
            },
        ]),
        mode: None,
        directories: None,
    };

    assert_eq!(
        serde_json::to_value(&add_rules).unwrap(),
        serde_json::json!({
            "type": "addRules",
            "destination": "localSettings",
            "behavior": "allow",
            "rules": [
                {"toolName": "Bash", "ruleContent": "npm *"},
                {"toolName": "Read"}
            ]
        })
    );

    let set_mode: PermissionUpdate = serde_json::from_value(serde_json::json!({
        "type": "setMode",
        "mode": "acceptEdits",
        "destination": "session"
    }))
    .unwrap();
    assert_eq!(set_mode.mode, Some(PermissionMode::AcceptEdits));
    assert!(set_mode.rules.is_none());

    let directories: PermissionUpdate = serde_json::from_value(serde_json::json!({
        "type": "addDirectories",
        "directories": ["/tmp/a", "/tmp/b"],
        "destination": "userSettings"
    }))
    .unwrap();
    assert_eq!(
        directories.directories,
        Some(vec!["/tmp/a".to_string(), "/tmp/b".to_string()])
    );
}

#[test]
fn message_content_blocks_match_python_contracts() {
    let text = ContentBlock::Text {
        text: "Hello, human!".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&text).unwrap(),
        serde_json::json!({"type": "text", "text": "Hello, human!"})
    );

    let thinking = ContentBlock::Thinking {
        thinking: "I'm thinking...".to_string(),
        signature: "sig-123".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&thinking).unwrap(),
        serde_json::json!({
            "type": "thinking",
            "thinking": "I'm thinking...",
            "signature": "sig-123"
        })
    );

    let tool_use = ContentBlock::ToolUse {
        id: "tool-123".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({"file_path": "/test.txt"})
            .as_object()
            .unwrap()
            .clone(),
    };
    assert_eq!(
        serde_json::to_value(&tool_use).unwrap(),
        serde_json::json!({
            "type": "tool_use",
            "id": "tool-123",
            "name": "Read",
            "input": {"file_path": "/test.txt"}
        })
    );
}

#[test]
fn default_and_builder_options_cover_python_option_contracts() {
    let default_options = ClaudeAgentOptions::default();
    assert!(default_options.allowed_tools.is_empty());
    assert!(default_options.system_prompt.is_none());
    assert!(default_options.permission_mode.is_none());
    assert!(!default_options.continue_conversation);
    assert!(default_options.disallowed_tools.is_empty());

    let options = ClaudeAgentOptions::builder()
        .allowed_tools(vec![
            "Read".to_string(),
            "Write".to_string(),
            "Edit".to_string(),
        ])
        .disallowed_tools(vec!["Bash".to_string()])
        .permission_mode(PermissionMode::BypassPermissions)
        .system_prompt("You are a helpful assistant.")
        .continue_conversation(true)
        .resume("session-123")
        .model("claude-sonnet-4-5")
        .permission_prompt_tool_name("CustomTool")
        .build();

    assert_eq!(options.allowed_tools, ["Read", "Write", "Edit"]);
    assert_eq!(options.disallowed_tools, ["Bash"]);
    assert_eq!(
        options.permission_mode,
        Some(PermissionMode::BypassPermissions)
    );
    assert_eq!(
        options.system_prompt.as_deref(),
        Some("You are a helpful assistant.")
    );
    assert!(options.continue_conversation);
    assert_eq!(options.resume.as_deref(), Some("session-123"));
    assert_eq!(options.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(
        options.permission_prompt_tool_name.as_deref(),
        Some("CustomTool")
    );
}

#[test]
fn agent_definition_serializes_with_cli_camelcase_keys() {
    let agent = AgentDefinition {
        description: "test".to_string(),
        prompt: "p".to_string(),
        tools: None,
        disallowed_tools: Some(vec!["Bash".to_string(), "Write".to_string()]),
        model: Some("claude-opus-4-5".to_string()),
        skills: Some(vec!["skill-a".to_string(), "skill-b".to_string()]),
        memory: Some(SettingSource::Project),
        mcp_servers: Some(vec![
            serde_json::json!("slack"),
            serde_json::json!({"local": {"command": "python", "args": ["server.py"]}}),
        ]),
        initial_prompt: Some("/review-pr 123".to_string()),
        max_turns: Some(10),
        background: None,
        effort: None,
        permission_mode: None,
    };
    let payload = serde_json::to_value(agent).unwrap();

    assert_eq!(
        payload["disallowedTools"],
        serde_json::json!(["Bash", "Write"])
    );
    assert!(payload.get("disallowed_tools").is_none());
    assert_eq!(payload["maxTurns"], 10);
    assert!(payload.get("max_turns").is_none());
    assert_eq!(payload["initialPrompt"], "/review-pr 123");
    assert!(payload.get("initial_prompt").is_none());
    assert_eq!(payload["mcpServers"][0], "slack");
    assert_eq!(payload["memory"], "project");
    assert_eq!(payload["model"], "claude-opus-4-5");
}

#[test]
fn mcp_status_types_accept_python_wire_shapes() {
    let connected: MCPServerStatus = serde_json::from_value(serde_json::json!({
        "name": "my-server",
        "status": "connected",
        "serverInfo": {"name": "my-server", "version": "1.2.3"},
        "config": {"type": "http", "url": "https://example.com"},
        "scope": "project",
        "tools": [{
            "name": "greet",
            "description": "Greet a user",
            "annotations": {
                "readOnly": true,
                "destructive": false,
                "openWorld": false
            }
        }]
    }))
    .unwrap();
    assert_eq!(connected.status, MCPServerConnectionStatus::Connected);
    assert_eq!(connected.server_info.unwrap().version, "1.2.3");
    assert!(connected.tools.unwrap()[0]
        .annotations
        .as_ref()
        .unwrap()
        .read_only
        .unwrap());

    let proxy: MCPServerStatus = serde_json::from_value(serde_json::json!({
        "name": "proxy-server",
        "status": "needs-auth",
        "config": {
            "type": "claudeai-proxy",
            "url": "https://claude.ai/proxy",
            "id": "proxy-abc"
        }
    }))
    .unwrap();
    assert!(matches!(
        proxy.config,
        Some(MCPServerStatusConfig::ClaudeAiProxy { id, .. }) if id == "proxy-abc"
    ));

    let response: MCPStatusResponse = serde_json::from_value(serde_json::json!({
        "mcpServers": [
            {"name": "a", "status": "connected"},
            {"name": "b", "status": "disabled"}
        ]
    }))
    .unwrap();
    assert_eq!(response.mcp_servers.len(), 2);
}
