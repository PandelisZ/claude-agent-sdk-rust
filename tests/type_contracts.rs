use claude_code_sdk_rust::{
    AgentDefinition, ClaudeAgentOptions, ContentBlock, EffortLevel, HookInput,
    MCPServerConnectionStatus, MCPServerStatus, MCPServerStatusConfig, MCPStatusResponse,
    NotificationHookSpecificOutput, PermissionMode, PermissionRuleValue, PermissionUpdate,
    PreCompactHookInput, PreToolUseHookInput, ServerToolName, SettingSource,
};

#[test]
fn permission_update_round_trips_python_wire_shapes() {
    let add_rules = PermissionUpdate {
        r#type: "addRules".into(),
        destination: Some("localSettings".into()),
        behavior: Some("allow".into()),
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
    assert_eq!(set_mode.mode, Some(PermissionMode::AcceptEdits.into()));
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

#[test]
fn effort_level_serializes_to_python_literal_strings() {
    // EffortLevel mirrors the upstream Literal["low","medium","high","xhigh","max"].
    for (level, wire) in [
        (EffortLevel::Low, "low"),
        (EffortLevel::Medium, "medium"),
        (EffortLevel::High, "high"),
        (EffortLevel::Xhigh, "xhigh"),
        (EffortLevel::Max, "max"),
    ] {
        assert_eq!(
            serde_json::to_value(level).unwrap(),
            serde_json::json!(wire)
        );
        assert_eq!(level.as_cli(), wire);
        let parsed: EffortLevel = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(parsed, level);
    }

    // The builder stores the typed effort on the options.
    let options = ClaudeAgentOptions::builder()
        .effort(EffortLevel::Xhigh)
        .build();
    assert_eq!(options.effort, Some(EffortLevel::Xhigh));
}

#[test]
fn server_tool_name_round_trips_and_is_forward_compatible() {
    // Known names map to typed variants on the wire.
    let known: ServerToolName = serde_json::from_value(serde_json::json!("web_search")).unwrap();
    assert_eq!(known, ServerToolName::WebSearch);
    assert_eq!(
        serde_json::to_value(&known).unwrap(),
        serde_json::json!("web_search")
    );
    assert_eq!(known.as_str(), "web_search");

    // Unknown names are preserved (forward-compatible) rather than failing.
    let unknown: ServerToolName = serde_json::from_value(serde_json::json!("future_tool")).unwrap();
    assert_eq!(unknown, ServerToolName::Other("future_tool".to_string()));
    assert_eq!(
        serde_json::to_value(&unknown).unwrap(),
        serde_json::json!("future_tool")
    );

    // A server_tool_use content block carries the typed name.
    let block: ContentBlock = serde_json::from_value(serde_json::json!({
        "type": "server_tool_use",
        "id": "srvtoolu_1",
        "name": "web_fetch",
        "input": {"url": "https://example.com"}
    }))
    .unwrap();
    match block {
        ContentBlock::ServerToolUse { name, .. } => assert_eq!(name, ServerToolName::WebFetch),
        other => panic!("expected server_tool_use, got {other:?}"),
    }
}

#[test]
fn hook_input_deserializes_python_wire_shapes() {
    // PreToolUse carries flattened base fields plus tool fields and is
    // discriminated by hook_event_name.
    let raw = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-1",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/work",
        "permission_mode": "default",
        "tool_name": "Bash",
        "tool_input": {"command": "ls"},
        "tool_use_id": "toolu_1",
        "agent_id": "agent-9"
    });
    let input: HookInput = serde_json::from_value(raw).unwrap();
    match input {
        HookInput::PreToolUse(PreToolUseHookInput {
            base,
            tool_name,
            tool_use_id,
            agent_id,
            ..
        }) => {
            assert_eq!(base.session_id, "sess-1");
            assert_eq!(base.cwd, "/work");
            assert_eq!(base.permission_mode.as_deref(), Some("default"));
            assert_eq!(tool_name, "Bash");
            assert_eq!(tool_use_id, "toolu_1");
            assert_eq!(agent_id.as_deref(), Some("agent-9"));
        }
        other => panic!("expected PreToolUse, got {other:?}"),
    }

    // PreCompact carries an explicit null custom_instructions.
    let compact: PreCompactHookInput = serde_json::from_value(serde_json::json!({
        "session_id": "sess-1",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/work",
        "trigger": "auto",
        "custom_instructions": null
    }))
    .unwrap();
    assert_eq!(compact.trigger, "auto");
    assert!(compact.custom_instructions.is_none());
}

#[test]
fn hook_specific_output_uses_camel_case_keys() {
    let output = NotificationHookSpecificOutput {
        additional_context: Some("note".to_string()),
    };
    assert_eq!(
        serde_json::to_value(&output).unwrap(),
        serde_json::json!({"additionalContext": "note"})
    );

    // Omitted optional fields do not appear on the wire.
    let empty = NotificationHookSpecificOutput::default();
    assert_eq!(serde_json::to_value(&empty).unwrap(), serde_json::json!({}));
}

// --- types workstream (upstream v0.2.189 leftovers) --------------------------

#[test]
fn sandbox_ignore_violations_is_a_command_pattern_map() {
    use claude_code_sdk_rust::{SandboxIgnoreViolations, SandboxSettings};

    let mut ignore = SandboxIgnoreViolations::new();
    ignore.insert("*".to_string(), vec!["/etc/hosts".to_string()]);
    let sandbox = SandboxSettings {
        enabled: Some(true),
        ignore_violations: Some(ignore),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&sandbox).unwrap(),
        serde_json::json!({"enabled": true, "ignoreViolations": {"*": ["/etc/hosts"]}})
    );

    let parsed: SandboxSettings = serde_json::from_value(serde_json::json!({
        "ignoreViolations": {"*": ["/etc/hosts"], "npm": ["EACCES", "/tmp"]}
    }))
    .unwrap();
    let parsed = parsed.ignore_violations.unwrap();
    assert_eq!(
        parsed["npm"],
        vec!["EACCES".to_string(), "/tmp".to_string()]
    );
    assert_eq!(parsed["*"], vec!["/etc/hosts".to_string()]);
}

#[test]
fn thinking_config_matches_python_typed_dicts() {
    use claude_code_sdk_rust::{ThinkingConfig, ThinkingDisplay};

    assert_eq!(
        serde_json::to_value(ThinkingConfig::adaptive()).unwrap(),
        serde_json::json!({"type": "adaptive"})
    );
    assert_eq!(
        serde_json::to_value(ThinkingConfig::enabled(4096).with_display(ThinkingDisplay::Omitted))
            .unwrap(),
        serde_json::json!({"type": "enabled", "budget_tokens": 4096, "display": "omitted"})
    );
    assert_eq!(
        serde_json::to_value(ThinkingConfig::disabled()).unwrap(),
        serde_json::json!({"type": "disabled"})
    );
    let parsed: ThinkingConfig = serde_json::from_value(serde_json::json!({
        "type": "enabled", "budget_tokens": 10, "display": "summarized"
    }))
    .unwrap();
    assert_eq!(
        parsed,
        ThinkingConfig::enabled(10).with_display(ThinkingDisplay::Summarized)
    );
    // camelCase input is still accepted.
    let legacy: ThinkingConfig =
        serde_json::from_value(serde_json::json!({"type": "enabled", "budgetTokens": 7})).unwrap();
    assert_eq!(legacy.budget_tokens, Some(7));
    // ThinkingDisplay is a closed literal like Python's.
    assert!(serde_json::from_value::<ThinkingDisplay>(serde_json::json!("full")).is_err());
}

#[test]
fn system_prompt_preset_matches_python_typed_dict() {
    use claude_code_sdk_rust::SystemPromptPreset;

    let preset = SystemPromptPreset::claude_code()
        .with_append("extra")
        .with_exclude_dynamic_sections(true)
        .with_snapshot(false);
    assert_eq!(
        serde_json::to_value(&preset).unwrap(),
        serde_json::json!({
            "type": "preset",
            "preset": "claude_code",
            "append": "extra",
            "exclude_dynamic_sections": true,
            "snapshot": false
        })
    );
    let parsed: SystemPromptPreset = serde_json::from_value(serde_json::json!({
        "type": "preset", "preset": "claude_code", "excludeDynamicSections": true
    }))
    .unwrap();
    assert_eq!(parsed.exclude_dynamic_sections, Some(true));
    assert_eq!(parsed.snapshot, None);

    let options = ClaudeAgentOptions::builder()
        .system_prompt_preset(SystemPromptPreset::claude_code().with_snapshot(true))
        .build();
    assert_eq!(options.system_prompt_snapshot(), Some(true));
    let options = ClaudeAgentOptions::builder()
        .system_prompt_preset(SystemPromptPreset::claude_code())
        .build();
    assert_eq!(options.system_prompt_snapshot(), None);
}

#[test]
fn agent_definition_effort_is_level_or_int() {
    use claude_code_sdk_rust::AgentEffort;

    let level = AgentDefinition {
        effort: Some(EffortLevel::High.into()),
        ..AgentDefinition::new("d", "p")
    };
    assert_eq!(
        serde_json::to_value(&level).unwrap(),
        serde_json::json!({"description": "d", "prompt": "p", "effort": "high"})
    );
    let tokens = AgentDefinition {
        effort: Some(AgentEffort::Tokens(2048)),
        ..AgentDefinition::new("d", "p")
    };
    assert_eq!(serde_json::to_value(&tokens).unwrap()["effort"], 2048);

    let parsed: AgentDefinition = serde_json::from_value(serde_json::json!({
        "description": "d", "prompt": "p", "effort": "xhigh"
    }))
    .unwrap();
    assert_eq!(parsed.effort, Some(AgentEffort::Level(EffortLevel::Xhigh)));
    let parsed: AgentDefinition = serde_json::from_value(serde_json::json!({
        "description": "d", "prompt": "p", "effort": 5
    }))
    .unwrap();
    assert_eq!(parsed.effort, Some(AgentEffort::Tokens(5)));
    assert!(
        serde_json::from_value::<AgentDefinition>(serde_json::json!({
            "description": "d", "prompt": "p", "effort": "extreme"
        }))
        .is_err()
    );
}

#[test]
fn permission_update_fields_are_typed_and_keep_unknown_values() {
    use claude_code_sdk_rust::{
        PermissionBehavior, PermissionUpdateDestination, PermissionUpdateMode, PermissionUpdateType,
    };

    let parsed: PermissionUpdate = serde_json::from_value(serde_json::json!({
        "type": "addRules",
        "rules": [{"toolName": "Bash"}],
        "behavior": "ask",
        "destination": "projectSettings"
    }))
    .unwrap();
    assert_eq!(parsed.r#type, PermissionUpdateType::AddRules);
    assert_eq!(parsed.behavior, Some(PermissionBehavior::Ask));
    assert_eq!(
        parsed.destination,
        Some(PermissionUpdateDestination::ProjectSettings)
    );

    // A suggestion from a newer CLI round-trips verbatim (Python passes the
    // raw strings through).
    let future = serde_json::json!({
        "type": "setMode",
        "mode": "superAuto",
        "destination": "cloudSettings"
    });
    let parsed: PermissionUpdate = serde_json::from_value(future.clone()).unwrap();
    assert_eq!(
        parsed.mode,
        Some(PermissionUpdateMode::Other("superAuto".into()))
    );
    assert_eq!(serde_json::to_value(&parsed).unwrap(), future);
    assert_eq!(parsed.to_dict().unwrap(), future);
    assert_eq!(PermissionUpdate::from_dict(&future).unwrap(), parsed);

    let known = PermissionUpdate {
        mode: Some(PermissionMode::Plan.into()),
        ..PermissionUpdate::new(PermissionUpdateType::SetMode)
    };
    assert_eq!(known.mode.as_ref().unwrap(), &PermissionMode::Plan);
    assert_eq!(
        known.mode.as_ref().unwrap().known(),
        Some(PermissionMode::Plan)
    );
    assert_eq!(
        known.to_dict().unwrap(),
        serde_json::json!({"type": "setMode", "destination": "session", "mode": "plan"})
    );
}

#[test]
fn stdio_mcp_server_config_type_is_optional_on_input() {
    use claude_code_sdk_rust::MCPServerConfig;

    let parsed: MCPServerConfig =
        serde_json::from_value(serde_json::json!({"command": "python", "args": ["s.py"]})).unwrap();
    assert!(matches!(parsed, MCPServerConfig::Stdio { ref command, .. } if command == "python"));
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        serde_json::json!({"type": "stdio", "command": "python", "args": ["s.py"]})
    );
    let http: MCPServerConfig = serde_json::from_value(serde_json::json!({
        "type": "http", "url": "https://x", "headers": {"A": "b"}
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&http).unwrap(),
        serde_json::json!({"type": "http", "url": "https://x", "headers": {"A": "b"}})
    );
    let sse: MCPServerConfig =
        serde_json::from_value(serde_json::json!({"type": "sse", "url": "https://y"})).unwrap();
    assert_eq!(
        serde_json::to_value(&sse).unwrap(),
        serde_json::json!({"type": "sse", "url": "https://y"})
    );
    assert!(
        serde_json::from_value::<MCPServerConfig>(serde_json::json!({"type": "bogus"})).is_err()
    );
}

#[test]
fn sdk_mcp_custom_server_registers_an_sdk_config() {
    use claude_code_sdk_rust::{MCPServerConfig, SdkMcpServer};
    use std::sync::Arc;

    struct Echo;
    #[async_trait::async_trait]
    impl SdkMcpServer for Echo {
        async fn handle_message(
            &self,
            message: serde_json::Value,
        ) -> Result<Option<serde_json::Value>, String> {
            Ok(Some(message))
        }
    }

    let options = ClaudeAgentOptions::builder()
        .sdk_mcp_custom_server("echo", Arc::new(Echo))
        .build();
    assert!(matches!(
        options.mcp_servers.get("echo"),
        Some(MCPServerConfig::Sdk { name }) if name == "echo"
    ));
    let server = options.sdk_mcp_servers.get("echo").expect("registered");
    assert!(server.is_custom());
}
