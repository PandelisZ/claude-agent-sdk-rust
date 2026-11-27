use super::cli_args::build_cli_args;
use super::transport::TransportOptions;
use crate::types::{
    ClaudeAgentOptions, PermissionMode, SandboxNetworkConfig, SandboxSettings, SdkBeta,
    ThinkingConfig, ThinkingConfigType,
};

#[test]
fn empty_tools_list_serializes_to_disable_all_tools_like_python_sdk() {
    let args = args_for(ClaudeAgentOptions::builder().tools(Vec::new()).build());

    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--tools")
            .map(|window| window[1].as_str()),
        Some("")
    );
}

fn args_for(options: ClaudeAgentOptions) -> Vec<String> {
    build_cli_args(&TransportOptions::from(&options)).expect("args")
}

#[test]
fn serializes_permission_mode_and_betas_as_cli_wire_values() {
    let options = ClaudeAgentOptions::builder()
        .permission_mode(PermissionMode::AcceptEdits)
        .betas(vec![SdkBeta::Context1M20250807])
        .build();

    let args = args_for(options);

    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--permission-mode")
            .map(|window| window[1].as_str()),
        Some("acceptEdits")
    );
    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--betas")
            .map(|window| window[1].as_str()),
        Some("context-1m-2025-08-07")
    );
}

#[test]
fn serializes_thinking_and_json_schema_like_python_sdk() {
    let mut schema = serde_json::Map::new();
    schema.insert(
        "type".to_string(),
        serde_json::Value::String("object".to_string()),
    );

    let mut output_format = serde_json::Map::new();
    output_format.insert(
        "type".to_string(),
        serde_json::Value::String("json_schema".to_string()),
    );
    output_format.insert(
        "schema".to_string(),
        serde_json::Value::Object(schema.clone()),
    );
    let expected_schema = serde_json::Value::Object(schema).to_string();

    let options = ClaudeAgentOptions::builder()
        .thinking(ThinkingConfig {
            r#type: ThinkingConfigType::Adaptive,
            budget_tokens: None,
            display: Some(crate::types::ThinkingDisplay::Summarized),
        })
        .output_format(output_format)
        .build();

    let args = args_for(options);

    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--thinking")
            .map(|window| window[1].as_str()),
        Some("adaptive")
    );
    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--thinking-display")
            .map(|window| window[1].as_str()),
        Some("summarized")
    );
    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--json-schema")
            .map(|window| window[1].as_str()),
        Some(expected_schema.as_str())
    );
}

#[test]
fn disabled_thinking_does_not_forward_display() {
    let options = ClaudeAgentOptions::builder()
        .thinking(ThinkingConfig {
            r#type: ThinkingConfigType::Disabled,
            budget_tokens: None,
            display: Some(crate::types::ThinkingDisplay::Omitted),
        })
        .build();

    let args = args_for(options);

    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--thinking")
            .map(|window| window[1].as_str()),
        Some("disabled")
    );
    assert!(!args.iter().any(|arg| arg == "--thinking-display"));
}

#[test]
fn serializes_current_python_sdk_session_and_control_flags() {
    let options = ClaudeAgentOptions::builder()
        .session_id("session-123")
        .task_budget_total(100_000)
        .include_hook_events(true)
        .strict_mcp_config(true)
        .setting_sources(vec![
            crate::types::SettingSource::User,
            crate::types::SettingSource::Project,
        ])
        .build();

    let args = args_for(options);

    assert!(args.iter().any(|arg| arg == "--session-id=session-123"));
    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--task-budget")
            .map(|window| window[1].as_str()),
        Some("100000")
    );
    assert!(args.iter().any(|arg| arg == "--include-hook-events"));
    assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
    assert!(args
        .iter()
        .any(|arg| arg == "--setting-sources=user,project"));
}

#[test]
fn can_use_tool_defaults_permission_prompt_tool_to_stdio() {
    let options = ClaudeAgentOptions::builder()
        .can_use_tool(|_, _, _| async { Ok(crate::types::PermissionResult::allow()) })
        .build();

    let args = args_for(options);

    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--permission-prompt-tool")
            .map(|window| window[1].as_str()),
        Some("stdio")
    );
}

#[test]
fn serializes_mcp_servers_raw_config_like_python_sdk() {
    let json_config = r#"{"mcpServers":{"server":{"type":"stdio","command":"test"}}}"#;
    let args = args_for(
        ClaudeAgentOptions::builder()
            .mcp_servers_config(json_config)
            .build(),
    );

    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--mcp-config")
            .map(|window| window[1].as_str()),
        Some(json_config)
    );

    let path = "/path/to/mcp-config.json";
    let args = args_for(
        ClaudeAgentOptions::builder()
            .mcp_servers_config(path)
            .build(),
    );
    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--mcp-config")
            .map(|window| window[1].as_str()),
        Some(path)
    );
}

#[test]
fn skills_config_adds_skill_tool_and_default_setting_sources() {
    let options = ClaudeAgentOptions::builder()
        .allowed_tools(vec!["Read".to_string()])
        .skills(vec!["reviewer".to_string(), "planner".to_string()])
        .build();

    let args = args_for(options);

    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--allowedTools")
            .map(|window| window[1].as_str()),
        Some("Read,Skill(reviewer),Skill(planner)")
    );
    assert!(args
        .iter()
        .any(|arg| arg == "--setting-sources=user,project"));
}

#[test]
fn skills_all_deduplicates_skill_tool_and_respects_explicit_setting_sources() {
    let options = ClaudeAgentOptions::builder()
        .allowed_tools(vec!["Skill".to_string()])
        .skills_all()
        .setting_sources(Vec::new())
        .build();

    let args = args_for(options);

    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--allowedTools")
            .map(|window| window[1].as_str()),
        Some("Skill")
    );
    assert!(args.iter().any(|arg| arg == "--setting-sources="));
}

#[test]
fn sandbox_settings_are_merged_into_settings_json() {
    let options = ClaudeAgentOptions::builder()
        .settings(r#"{"permissions":{"allow":["Read"]}}"#)
        .sandbox(SandboxSettings {
            enabled: Some(true),
            auto_allow_bash_if_sandboxed: Some(true),
            network: Some(SandboxNetworkConfig {
                allow_local_binding: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        })
        .build();

    let args = args_for(options);
    let settings = args
        .windows(2)
        .find(|window| window[0] == "--settings")
        .map(|window| window[1].as_str())
        .expect("settings argument");
    let settings: serde_json::Value = serde_json::from_str(settings).expect("settings json");

    assert_eq!(settings["permissions"]["allow"][0], "Read");
    assert_eq!(settings["sandbox"]["enabled"], true);
    assert_eq!(
        settings["sandbox"]["autoAllowBashIfSandboxed"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(settings["sandbox"]["network"]["allowLocalBinding"], true);
}

#[test]
fn unsupported_plugin_type_is_rejected() {
    let options = ClaudeAgentOptions::builder()
        .plugin(crate::types::SDKPluginConfig {
            r#type: "remote".to_string(),
            path: "/tmp/plugin".to_string(),
        })
        .build();

    let err = build_cli_args(&TransportOptions::from(&options))
        .expect_err("unsupported plugin type should fail");
    assert!(err.to_string().contains("Unsupported plugin type"));
}

#[test]
fn session_store_enables_session_mirror_flag() {
    let options = ClaudeAgentOptions::builder()
        .session_store(crate::session_store::InMemorySessionStore::new())
        .build();

    let args = args_for(options);

    assert!(args.iter().any(|arg| arg == "--session-mirror"));
}

#[test]
fn serializes_effort_level_as_cli_wire_value() {
    use crate::types::EffortLevel;

    let args = args_for(
        ClaudeAgentOptions::builder()
            .effort(EffortLevel::Xhigh)
            .build(),
    );

    assert_eq!(
        args.windows(2)
            .find(|window| window[0] == "--effort")
            .map(|window| window[1].as_str()),
        Some("xhigh")
    );
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].as_str())
}

#[test]
fn resume_and_session_id_use_single_equals_token_and_skip_empty() {
    let args = args_for(
        ClaudeAgentOptions::builder()
            .resume("--dangerous")
            .session_id("abc")
            .build(),
    );
    assert!(args.iter().any(|a| a == "--resume=--dangerous"));
    assert!(args.iter().any(|a| a == "--session-id=abc"));
    assert!(!args.iter().any(|a| a == "--resume" || a == "--session-id"));

    let args = args_for(
        ClaudeAgentOptions::builder()
            .resume("")
            .session_id("")
            .build(),
    );
    assert!(!args
        .iter()
        .any(|a| a.starts_with("--resume") || a.starts_with("--session-id")));
}

#[test]
fn resume_session_at_and_drops_turn_flags_like_python_sdk() {
    let args = args_for(
        ClaudeAgentOptions::builder()
            .resume("s1")
            .fork_session(true)
            .resume_session_at("msg-uuid")
            .resume_drops_turn("prompt-uuid")
            .session_store(crate::session_store::InMemorySessionStore::new())
            .build(),
    );
    let fork = args.iter().position(|a| a == "--fork-session").unwrap();
    let at = args
        .iter()
        .position(|a| a == "--resume-session-at=msg-uuid")
        .unwrap();
    let drops = args
        .iter()
        .position(|a| a == "--resume-drops-turn=prompt-uuid")
        .unwrap();
    let mirror = args.iter().position(|a| a == "--session-mirror").unwrap();
    assert!(fork < at && at < drops && drops < mirror);

    // Empty resume_session_at is skipped; empty resume_drops_turn is forwarded.
    let args = args_for(
        ClaudeAgentOptions::builder()
            .resume_session_at("")
            .resume_drops_turn("")
            .build(),
    );
    assert!(!args.iter().any(|a| a.starts_with("--resume-session-at")));
    assert!(args.iter().any(|a| a == "--resume-drops-turn="));
}

#[test]
fn windows_metacharacter_check_matches_python_message() {
    use super::cli_args::reject_windows_cmd_metacharacters;

    assert!(reject_windows_cmd_metacharacters("resume", "a&b|c", false).is_ok());
    assert!(reject_windows_cmd_metacharacters("resume", "plain-uuid", true).is_ok());
    let err = reject_windows_cmd_metacharacters("session_id", "x|y&z&\n", true).unwrap_err();
    assert_eq!(
        err.to_string(),
        "session_id value 'x|y&z&\\n' contains characters that are unsafe to pass on a Windows command line: ['\\n', '&', '|']"
    );
}

#[test]
fn extra_args_keep_insertion_order_position_and_equals_for_dash_values() {
    let args = args_for(
        ClaudeAgentOptions::builder()
            .plugin(crate::types::SDKPluginConfig {
                r#type: "local".to_string(),
                path: "/p".to_string(),
            })
            .extra_arg("zeta", None)
            .extra_arg("alpha", Some("value".to_string()))
            .extra_arg("dash", Some("-x".to_string()))
            .extra_arg("zeta", Some("2".to_string()))
            .effort(crate::types::EffortLevel::High)
            .build(),
    );
    let plugin = args.iter().position(|a| a == "--plugin-dir").unwrap();
    let start = plugin + 2;
    assert_eq!(
        &args[start..start + 5],
        &["--zeta", "2", "--alpha", "value", "--dash=-x"]
    );
    assert_eq!(args[start + 5], "--effort");
}

#[test]
fn argv_order_matches_python_build_command() {
    let options = ClaudeAgentOptions::builder()
        .system_prompt("sys")
        .tools(vec!["Read".to_string()])
        .allowed_tools(vec!["Read".to_string()])
        .max_turns(2)
        .max_budget_usd(1.0)
        .disallowed_tools(vec!["Bash".to_string()])
        .task_budget_total(10)
        .model("m")
        .fallback_model("f")
        .betas(vec![SdkBeta::Context1M20250807])
        .permission_prompt_tool_name("stdio")
        .permission_mode(PermissionMode::Plan)
        .continue_conversation(true)
        .resume("r")
        .session_id("s")
        .settings("{}")
        .add_dir("/d")
        .include_partial_messages(true)
        .fork_session(true)
        .setting_sources(vec![crate::types::SettingSource::User])
        .effort(crate::types::EffortLevel::Low)
        .build();
    let args = args_for(options);
    let flags: Vec<&str> = args
        .iter()
        .filter(|a| a.starts_with("--"))
        .map(|a| a.split('=').next().unwrap())
        .collect();
    assert_eq!(
        flags,
        vec![
            "--output-format",
            "--verbose",
            "--system-prompt",
            "--tools",
            "--allowedTools",
            "--max-turns",
            "--max-budget-usd",
            "--disallowedTools",
            "--task-budget",
            "--model",
            "--fallback-model",
            "--betas",
            "--permission-prompt-tool",
            "--permission-mode",
            "--continue",
            "--resume",
            "--session-id",
            "--settings",
            "--add-dir",
            "--include-partial-messages",
            "--fork-session",
            "--setting-sources",
            "--effort",
            "--input-format",
        ]
    );
    assert_eq!(flag_value(&args, "--max-budget-usd"), Some("1.0"));
    assert_eq!(&args[args.len() - 2..], &["--input-format", "stream-json"]);
}

#[test]
fn truthiness_gates_match_python_sdk() {
    let args = args_for(
        ClaudeAgentOptions::builder()
            .max_turns(0)
            .model("")
            .fallback_model("")
            .permission_prompt_tool_name("")
            .settings("")
            .mcp_servers_config("")
            .build(),
    );
    for flag in [
        "--max-turns",
        "--model",
        "--fallback-model",
        "--permission-prompt-tool",
        "--settings",
        "--mcp-config",
    ] {
        assert!(!args.iter().any(|a| a == flag), "{flag} should be omitted");
    }
    assert_eq!(
        flag_value(
            &args_for(ClaudeAgentOptions::builder().max_budget_usd(0.25).build()),
            "--max-budget-usd"
        ),
        Some("0.25")
    );
}

#[test]
fn any_tools_preset_maps_to_default_and_empty_plugin_path_is_forwarded() {
    let args = args_for(
        ClaudeAgentOptions::builder()
            .tools_preset(crate::types::ToolsPreset {
                r#type: "preset".to_string(),
                preset: "something_else".to_string(),
            })
            .plugin(crate::types::SDKPluginConfig {
                r#type: "local".to_string(),
                path: String::new(),
            })
            .build(),
    );
    assert_eq!(flag_value(&args, "--tools"), Some("default"));
    assert_eq!(flag_value(&args, "--plugin-dir"), Some(""));
}

#[test]
fn custom_system_prompt_uses_system_prompt_flag_and_setters_are_exclusive() {
    use crate::types::{SystemPromptCustom, SystemPromptFile, SystemPromptPreset};

    let options = ClaudeAgentOptions::builder()
        .system_prompt_file(SystemPromptFile {
            r#type: "file".to_string(),
            path: "/p.md".to_string(),
        })
        .system_prompt_custom(SystemPromptCustom::new("be terse").with_snapshot(false))
        .build();
    assert!(options.system_prompt_file.is_none());
    let args = args_for(options.clone());
    assert_eq!(flag_value(&args, "--system-prompt"), Some("be terse"));
    assert!(!args.iter().any(|a| a == "--system-prompt-file"));
    assert_eq!(options.system_prompt_snapshot(), Some(false));
    assert_eq!(options.exclude_dynamic_sections(), None);

    // A file prompt overriding a preset no longer leaks excludeDynamicSections.
    let options = ClaudeAgentOptions::builder()
        .system_prompt_preset(SystemPromptPreset {
            r#type: "preset".to_string(),
            preset: "claude_code".to_string(),
            append: Some("extra".to_string()),
            exclude_dynamic_sections: Some(true),
            snapshot: None,
        })
        .build();
    assert_eq!(options.exclude_dynamic_sections(), Some(true));
    assert_eq!(
        flag_value(&args_for(options.clone()), "--append-system-prompt"),
        Some("extra")
    );
    let mut overridden = options;
    overridden.system_prompt_file = Some(SystemPromptFile {
        r#type: "file".to_string(),
        path: "/p.md".to_string(),
    });
    assert_eq!(overridden.exclude_dynamic_sections(), None);
    assert_eq!(
        flag_value(&args_for(overridden), "--system-prompt-file"),
        Some("/p.md")
    );
}

#[test]
fn skill_names_are_validated_like_python_sdk() {
    let cases: &[(&str, &str)] = &[
        ("", "Skill names must be non-empty strings"),
        ("  ", "Skill names must be non-empty strings"),
        (
            " a",
            "Invalid skill name ' a': leading or trailing whitespace",
        ),
        ("a,b", "Invalid skill name 'a,b': parentheses, commas"),
        ("a(b)", "parentheses, commas, control characters"),
        ("a\u{7}b", "parentheses, commas, control characters"),
        ("a\u{feff}", "parentheses, commas, control characters"),
        (
            "*",
            "Invalid skill name '*': use skills=\"all\" to enable every skill.",
        ),
        ("plugin:*", "wildcard-suffix names are not allowed"),
        ("a *", "wildcard-suffix names are not allowed"),
        ("/review", "skill names may not start with '/'"),
        ("a\\\\b", "consecutive backslashes are not allowed"),
        ("a\\", "names may not end with an unpaired backslash"),
    ];
    for (name, expected) in cases {
        let options = ClaudeAgentOptions::builder()
            .skills(vec![name.to_string()])
            .build();
        let err = build_cli_args(&TransportOptions::from(&options))
            .expect_err("invalid skill name should fail");
        assert!(
            err.to_string().contains(expected),
            "{name:?}: {err} should contain {expected}"
        );
    }
    assert!(build_cli_args(&TransportOptions::from(
        &ClaudeAgentOptions::builder()
            .skills(vec!["plugin:skill".to_string(), "my-skill".to_string()])
            .build()
    ))
    .is_ok());
}

fn enabled_sandbox() -> SandboxSettings {
    SandboxSettings {
        enabled: Some(true),
        ..Default::default()
    }
}

#[test]
fn sandbox_settings_fail_closed_when_settings_cannot_load() {
    for settings in ["{not json}", "missing-settings-file.json", "[1]"] {
        let options = ClaudeAgentOptions::builder()
            .settings(settings)
            .sandbox(enabled_sandbox())
            .cwd("/definitely/not/a/dir")
            .build();
        let err = build_cli_args(&TransportOptions::from(&options))
            .expect_err("unloadable settings must fail");
        assert!(
            err.to_string().contains("Failed to load settings: "),
            "{settings}: {err}"
        );
        assert!(matches!(
            err,
            crate::error::ClaudeSDKError::CLIConnection(_)
        ));
    }
}

#[test]
fn sandbox_settings_file_resolves_against_cwd_and_accepts_bom() {
    let dir = std::env::temp_dir().join(format!("claude-rust-settings-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("settings.json"),
        "\u{feff}{\"permissions\":{\"allow\":[\"Read\"]}}",
    )
    .unwrap();

    let options = ClaudeAgentOptions::builder()
        .settings("  settings.json  ")
        .sandbox(enabled_sandbox())
        .cwd(dir.to_string_lossy().to_string())
        .build();
    let args = args_for(options);
    let settings: serde_json::Value =
        serde_json::from_str(flag_value(&args, "--settings").unwrap()).unwrap();
    assert_eq!(settings["permissions"]["allow"][0], "Read");
    assert_eq!(settings["sandbox"]["enabled"], true);
    let _ = std::fs::remove_dir_all(&dir);

    // Without sandbox, settings pass through untouched.
    let args = args_for(ClaudeAgentOptions::builder().settings("x.json").build());
    assert_eq!(flag_value(&args, "--settings"), Some("x.json"));
}

#[test]
fn verbatim_prompts_stamps_client_composed() {
    let message = serde_json::json!({
        "type": "user",
        "session_id": "",
        "message": {"role": "user", "content": "hi"},
        "parent_tool_use_id": null,
        "client_composed": false,
    });
    let off = ClaudeAgentOptions::builder().build();
    assert_eq!(off.stamp_user_message(message.clone()), message);

    let on = ClaudeAgentOptions::builder().verbatim_prompts(true).build();
    let stamped = on.stamp_user_message(message);
    assert_eq!(stamped["client_composed"], true);
    assert_eq!(stamped["message"]["content"], "hi");
}
