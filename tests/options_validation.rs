use claude_code_sdk_rust::{query_messages, ClaudeAgentClient, ClaudeAgentOptions};

#[test]
fn can_use_tool_rejects_explicit_permission_prompt_tool_name() {
    let options = ClaudeAgentOptions::builder()
        .can_use_tool(|_, _, _| async { Ok(claude_code_sdk_rust::PermissionResult::allow()) })
        .permission_prompt_tool_name("custom")
        .build();

    let err = ClaudeAgentClient::new(options).expect_err("conflicting options should fail fast");
    assert!(err
        .to_string()
        .contains("can_use_tool callback cannot be used"));
}

#[tokio::test]
async fn query_messages_accepts_can_use_tool_with_string_prompt() {
    // Upstream #1204: a string prompt keeps stdin open for the permission
    // callback, so the old "requires streaming mode" guard is gone. The
    // query gets as far as looking for the CLI.
    let options = ClaudeAgentOptions::builder()
        .can_use_tool(|_, _, _| async { Ok(claude_code_sdk_rust::PermissionResult::allow()) })
        .cli_path("/nonexistent/claude-for-sdk-test")
        .build();

    let err = query_messages("hello", Some(options))
        .await
        .expect_err("no CLI at that path");
    assert!(!err.to_string().contains("streaming mode"), "{err}");
}

#[test]
fn new_upstream_options_default_off_and_builder_sets_them() {
    let defaults = ClaudeAgentOptions::default();
    assert!(defaults.resume_session_at.is_none());
    assert!(defaults.resume_drops_turn.is_none());
    assert!(defaults.client_app.is_none());
    assert!(!defaults.verbatim_prompts);
    assert!(!defaults.forward_subagent_text);
    assert!(defaults.system_prompt_custom.is_none());
    assert!(defaults.extra_args.is_empty());

    let options = ClaudeAgentOptions::builder()
        .resume_session_at("msg")
        .resume_drops_turn("")
        .client_app("my-app/1.0.0")
        .verbatim_prompts(true)
        .forward_subagent_text(true)
        .extra_arg("b", Some("1".to_string()))
        .extra_arg("a", None)
        .build();
    assert_eq!(options.resume_session_at.as_deref(), Some("msg"));
    assert_eq!(options.resume_drops_turn.as_deref(), Some(""));
    assert_eq!(options.client_app.as_deref(), Some("my-app/1.0.0"));
    assert!(options.verbatim_prompts);
    assert!(options.forward_subagent_text);
    assert_eq!(
        options.extra_args,
        vec![
            ("b".to_string(), Some("1".to_string())),
            ("a".to_string(), None)
        ]
    );
}

#[test]
fn system_prompt_union_matches_python_json_shapes() {
    use claude_code_sdk_rust::{SystemPrompt, SystemPromptCustom};

    let custom = SystemPromptCustom::new("be terse").with_snapshot(true);
    assert_eq!(
        serde_json::to_value(&custom).unwrap(),
        serde_json::json!({"type": "custom", "prompt": "be terse", "snapshot": true})
    );
    assert_eq!(
        serde_json::to_value(SystemPromptCustom::new("p")).unwrap(),
        serde_json::json!({"type": "custom", "prompt": "p"})
    );

    let text: SystemPrompt = serde_json::from_value(serde_json::json!("plain")).unwrap();
    assert!(matches!(text, SystemPrompt::Text(ref t) if t == "plain"));
    let parsed: SystemPrompt =
        serde_json::from_value(serde_json::json!({"type": "custom", "prompt": "x"})).unwrap();
    assert!(matches!(parsed, SystemPrompt::Custom(_)));
    let parsed: SystemPrompt =
        serde_json::from_value(serde_json::json!({"type": "file", "path": "/p.md"})).unwrap();
    assert!(matches!(parsed, SystemPrompt::File(_)));

    let options = ClaudeAgentOptions::builder()
        .system_prompt("first")
        .system_prompt_config(custom)
        .build();
    assert!(options.system_prompt.is_none());
    assert_eq!(options.system_prompt_snapshot(), Some(true));
    assert!(matches!(
        options.active_system_prompt(),
        Some(SystemPrompt::Custom(_))
    ));
}
