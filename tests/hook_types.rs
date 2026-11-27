//! Parity tests for hook and permission types (upstream Python v0.2.189
//! `types.py`). Each test pins the exact Python/CLI JSON shape.

use claude_code_sdk_rust::{
    can_use_tool_shadowed_warning, AsyncHookJSONOutput, BackgroundTaskSummary, CanUseToolCallback,
    CanUseToolShadowedWarning, ClaudeAgentOptions, ClaudeAgentOptionsBuilder, ElicitationAction,
    ElicitationHookSpecificOutput, HookCallback, HookDecision, HookEffort, HookEvent, HookInput,
    HookJSONOutput, HookMatcher, HookOnFailure, HookPermissionDecision, HookSpecificOutput,
    MessageDisplayHookSpecificOutput, PermissionBehavior, PermissionDeniedHookSpecificOutput,
    PermissionMode, PermissionResult, PermissionRuleValue, PermissionUpdate,
    PermissionUpdateDestination, PermissionUpdateType, PostToolUseHookSpecificOutput,
    PreModelSwitchHookSpecificOutput, PreToolUseHookSpecificOutput, SessionCronSummary,
    SessionStartHookSpecificOutput, SkillsConfig, SyncHookJSONOutput,
    UserPromptSubmitHookSpecificOutput, WorktreeCreateHookSpecificOutput,
};
use serde_json::json;

fn noop_hook() -> HookCallback {
    HookCallback::new(|_, _, _| async { Ok(json!({})) })
}

fn allow_all() -> CanUseToolCallback {
    CanUseToolCallback::new(|_, _, _| async { Ok(PermissionResult::allow()) })
}

// --- hook inputs -----------------------------------------------------------

#[test]
fn base_hook_input_carries_prompt_id_and_effort() {
    let raw = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s",
        "transcript_path": "/t",
        "cwd": "/w",
        "prompt_id": "p-1",
        "effort": {"level": "high"},
        "tool_name": "Bash",
        "tool_input": {},
        "tool_response": "ok",
        "tool_use_id": "tu",
        "duration_ms": 42
    });
    let HookInput::PostToolUse(input) = serde_json::from_value::<HookInput>(raw.clone()).unwrap()
    else {
        panic!("expected PostToolUse");
    };
    assert_eq!(input.base.prompt_id.as_deref(), Some("p-1"));
    assert_eq!(
        input.base.effort,
        Some(HookEffort {
            level: "high".into()
        })
    );
    assert_eq!(input.duration_ms, Some(42));
    assert_eq!(
        serde_json::to_value(HookInput::PostToolUse(input)).unwrap(),
        raw
    );

    // Absent optional fields stay absent.
    let failure: HookInput = serde_json::from_value(json!({
        "hook_event_name": "PostToolUseFailure",
        "session_id": "s", "transcript_path": "/t", "cwd": "/w",
        "tool_name": "Bash", "tool_input": {}, "tool_use_id": "tu", "error": "boom",
        "duration_ms": 7
    }))
    .unwrap();
    let HookInput::PostToolUseFailure(failure) = failure else {
        panic!("expected PostToolUseFailure");
    };
    assert_eq!(failure.duration_ms, Some(7));
    assert!(failure.base.prompt_id.is_none());
    assert!(failure.base.effort.is_none());
}

#[test]
fn stop_inputs_carry_background_tasks_and_session_crons() {
    let raw = json!({
        "hook_event_name": "Stop",
        "session_id": "s", "transcript_path": "/t", "cwd": "/w",
        "stop_hook_active": false,
        "last_assistant_message": "done",
        "background_tasks": [
            {"id": "b1", "type": "shell", "status": "running", "description": "build",
             "command": "make"},
            {"id": "b2", "type": "subagent", "status": "running", "description": "d",
             "agent_type": "reviewer"},
            {"id": "b3", "type": "monitor", "status": "running", "description": "d",
             "server": "srv", "tool": "watch"},
            {"id": "b4", "type": "workflow", "status": "pending", "description": "d",
             "name": "wf"}
        ],
        "session_crons": [
            {"id": "c1", "schedule": "0 9 * * 1-5", "recurring": true, "prompt": "standup"}
        ]
    });
    let HookInput::Stop(stop) = serde_json::from_value::<HookInput>(raw.clone()).unwrap() else {
        panic!("expected Stop");
    };
    assert_eq!(stop.last_assistant_message.as_deref(), Some("done"));
    let tasks = stop.background_tasks.as_ref().unwrap();
    assert_eq!(tasks[0].r#type, "shell");
    assert_eq!(tasks[0].command.as_deref(), Some("make"));
    assert_eq!(tasks[1].agent_type.as_deref(), Some("reviewer"));
    assert_eq!(tasks[3].name.as_deref(), Some("wf"));
    assert_eq!(
        stop.session_crons.as_ref().unwrap()[0],
        SessionCronSummary {
            id: "c1".into(),
            schedule: "0 9 * * 1-5".into(),
            recurring: true,
            prompt: "standup".into(),
        }
    );
    assert_eq!(serde_json::to_value(HookInput::Stop(stop)).unwrap(), raw);

    let sub: HookInput = serde_json::from_value(json!({
        "hook_event_name": "SubagentStop",
        "session_id": "s", "transcript_path": "/t", "cwd": "/w",
        "stop_hook_active": true, "agent_id": "a", "agent_transcript_path": "/a",
        "agent_type": "x",
        "last_assistant_message": "bye",
        "background_tasks": [],
        "session_crons": []
    }))
    .unwrap();
    let HookInput::SubagentStop(sub) = sub else {
        panic!("expected SubagentStop");
    };
    assert_eq!(sub.last_assistant_message.as_deref(), Some("bye"));
    assert_eq!(sub.background_tasks, Some(vec![]));
    assert_eq!(sub.session_crons, Some(vec![]));

    let minimal = BackgroundTaskSummary {
        id: "b".into(),
        r#type: "shell".into(),
        status: "running".into(),
        description: "d".into(),
        command: None,
        agent_type: None,
        server: None,
        tool: None,
        name: None,
    };
    assert_eq!(
        serde_json::to_value(&minimal).unwrap(),
        json!({"id": "b", "type": "shell", "status": "running", "description": "d"})
    );
}

// --- hook events / matcher -------------------------------------------------

#[test]
fn hook_event_names_match_python_literal() {
    let all = [
        (HookEvent::PreToolUse, "PreToolUse"),
        (HookEvent::PostToolUse, "PostToolUse"),
        (HookEvent::PostToolUseFailure, "PostToolUseFailure"),
        (HookEvent::UserPromptSubmit, "UserPromptSubmit"),
        (HookEvent::Stop, "Stop"),
        (HookEvent::SubagentStop, "SubagentStop"),
        (HookEvent::PreCompact, "PreCompact"),
        (HookEvent::Notification, "Notification"),
        (HookEvent::SubagentStart, "SubagentStart"),
        (HookEvent::PermissionRequest, "PermissionRequest"),
    ];
    for (event, name) in all {
        assert_eq!(event.as_str(), name);
        assert_eq!(String::from(event), name);
        assert_eq!(serde_json::to_value(event).unwrap(), json!(name));
    }

    // The builder accepts both typed events and raw strings.
    let options = ClaudeAgentOptionsBuilder::default()
        .hook(HookEvent::PreToolUse, HookMatcher::new(noop_hook()))
        .hook("SessionStart", HookMatcher::new(noop_hook()))
        .build();
    assert!(options.hooks.contains_key("PreToolUse"));
    assert!(options.hooks.contains_key("SessionStart"));
}

#[test]
fn hook_matcher_on_failure_builder_and_wire_values() {
    let matcher = HookMatcher::new(noop_hook()).on_failure(HookOnFailure::Block);
    assert_eq!(matcher.on_failure, Some(HookOnFailure::Block));
    assert!(matcher.blocks_on_failure());

    let cont = HookMatcher::new(noop_hook()).on_failure("continue");
    assert_eq!(cont.on_failure, Some(HookOnFailure::Continue));
    assert!(!cont.blocks_on_failure());
    assert!(!HookMatcher::new(noop_hook()).blocks_on_failure());

    // Unknown values pass through and count as block (as the CLI reads them).
    let other = HookMatcher::new(noop_hook()).on_failure("fail-closed");
    assert_eq!(
        other.on_failure,
        Some(HookOnFailure::Other("fail-closed".into()))
    );
    assert!(other.blocks_on_failure());

    assert_eq!(
        serde_json::to_value(HookOnFailure::Block).unwrap(),
        json!("block")
    );
    assert_eq!(
        serde_json::to_value(HookOnFailure::Continue).unwrap(),
        json!("continue")
    );
    assert_eq!(
        serde_json::from_value::<HookOnFailure>(json!("x")).unwrap(),
        HookOnFailure::Other("x".into())
    );
}

// --- hook-specific outputs ---------------------------------------------------

#[test]
fn hook_specific_output_serializes_hook_event_name() {
    let pre: HookSpecificOutput = PreToolUseHookSpecificOutput {
        permission_decision: Some(HookPermissionDecision::Defer),
        permission_decision_reason: Some("why".into()),
        updated_input: Some(serde_json::Map::from_iter([("a".to_string(), json!(1))])),
        additional_context: Some("ctx".into()),
    }
    .into();
    assert_eq!(pre.hook_event_name(), "PreToolUse");
    assert_eq!(
        serde_json::to_value(&pre).unwrap(),
        json!({
            "hookEventName": "PreToolUse",
            "permissionDecision": "defer",
            "permissionDecisionReason": "why",
            "updatedInput": {"a": 1},
            "additionalContext": "ctx"
        })
    );

    let post = HookSpecificOutput::from(PostToolUseHookSpecificOutput {
        additional_context: None,
        updated_tool_output: Some(json!({"stdout": "x"})),
        updated_mcp_tool_output: Some(json!([1])),
    });
    assert_eq!(
        serde_json::to_value(&post).unwrap(),
        json!({
            "hookEventName": "PostToolUse",
            "updatedToolOutput": {"stdout": "x"},
            "updatedMCPToolOutput": [1]
        })
    );

    let prompt = HookSpecificOutput::from(UserPromptSubmitHookSpecificOutput {
        additional_context: None,
        session_title: Some("t".into()),
        suppress_original_prompt: Some(true),
    });
    assert_eq!(
        serde_json::to_value(&prompt).unwrap(),
        json!({"hookEventName": "UserPromptSubmit", "sessionTitle": "t",
               "suppressOriginalPrompt": true})
    );

    let start = HookSpecificOutput::from(SessionStartHookSpecificOutput {
        additional_context: Some("c".into()),
        initial_user_message: Some("hi".into()),
        session_title: None,
        watch_paths: Some(vec!["/x".into()]),
        reload_skills: Some(true),
    });
    assert_eq!(
        serde_json::to_value(&start).unwrap(),
        json!({"hookEventName": "SessionStart", "additionalContext": "c",
               "initialUserMessage": "hi", "watchPaths": ["/x"], "reloadSkills": true})
    );

    let denied = HookSpecificOutput::from(PermissionDeniedHookSpecificOutput { retry: Some(true) });
    assert_eq!(
        serde_json::to_value(&denied).unwrap(),
        json!({"hookEventName": "PermissionDenied", "retry": true})
    );

    let switch = HookSpecificOutput::from(PreModelSwitchHookSpecificOutput {
        permission_decision: Some(PermissionBehavior::Deny),
        permission_decision_reason: None,
    });
    assert_eq!(
        serde_json::to_value(&switch).unwrap(),
        json!({"hookEventName": "PreModelSwitch", "permissionDecision": "deny"})
    );

    let elicit = HookSpecificOutput::from(ElicitationHookSpecificOutput {
        action: Some(ElicitationAction::Accept),
        content: Some(serde_json::Map::from_iter([("k".to_string(), json!("v"))])),
    });
    assert_eq!(
        serde_json::to_value(&elicit).unwrap(),
        json!({"hookEventName": "Elicitation", "action": "accept", "content": {"k": "v"}})
    );

    let worktree = HookSpecificOutput::from(WorktreeCreateHookSpecificOutput {
        worktree_path: "/wt".into(),
    });
    assert_eq!(
        serde_json::to_value(&worktree).unwrap(),
        json!({"hookEventName": "WorktreeCreate", "worktreePath": "/wt"})
    );

    let display = HookSpecificOutput::from(MessageDisplayHookSpecificOutput {
        display_content: Some("shown".into()),
    });
    assert_eq!(
        serde_json::to_value(&display).unwrap(),
        json!({"hookEventName": "MessageDisplay", "displayContent": "shown"})
    );

    // Every remaining event name round-trips through the tag.
    for name in [
        "PostToolUseFailure",
        "Notification",
        "SubagentStart",
        "UserPromptExpansion",
        "PostToolBatch",
        "Stop",
        "SubagentStop",
        "PostModelSwitch",
        "ElicitationResult",
        "CwdChanged",
        "FileChanged",
    ] {
        let value = json!({"hookEventName": name});
        let parsed: HookSpecificOutput = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(parsed.hook_event_name(), name);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
    }
    let permission: HookSpecificOutput = serde_json::from_value(json!({
        "hookEventName": "PermissionRequest",
        "decision": {"behavior": "allow"}
    }))
    .unwrap();
    assert_eq!(permission.hook_event_name(), "PermissionRequest");
}

#[test]
fn hook_json_output_uses_cli_keys() {
    let sync = SyncHookJSONOutput {
        continue_: Some(false),
        suppress_output: Some(true),
        stop_reason: Some("stop".into()),
        decision: Some(HookDecision::Block),
        system_message: Some("warn".into()),
        reason: Some("because".into()),
        hook_specific_output: Some(
            PreToolUseHookSpecificOutput {
                permission_decision: Some(HookPermissionDecision::Deny),
                ..Default::default()
            }
            .into(),
        ),
    };
    let value: serde_json::Value = HookJSONOutput::from(sync).into();
    assert_eq!(
        value,
        json!({
            "continue": false,
            "suppressOutput": true,
            "stopReason": "stop",
            "decision": "block",
            "systemMessage": "warn",
            "reason": "because",
            "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny"}
        })
    );
    assert_eq!(
        serde_json::Value::from(SyncHookJSONOutput::default()),
        json!({})
    );

    let async_out = AsyncHookJSONOutput {
        async_: true,
        async_timeout: Some(5000),
    };
    assert_eq!(
        serde_json::Value::from(async_out),
        json!({"async": true, "asyncTimeout": 5000})
    );
    assert_eq!(
        serde_json::Value::from(AsyncHookJSONOutput::default()),
        json!({"async": true})
    );

    assert!(matches!(
        serde_json::from_value::<HookJSONOutput>(json!({"async": true})).unwrap(),
        HookJSONOutput::Async(_)
    ));
    assert!(matches!(
        serde_json::from_value::<HookJSONOutput>(json!({"continue": true})).unwrap(),
        HookJSONOutput::Sync(_)
    ));
}

// --- permission updates ------------------------------------------------------

fn update(kind: &str) -> PermissionUpdate {
    PermissionUpdate {
        r#type: kind.to_string(),
        rules: None,
        behavior: None,
        mode: None,
        directories: None,
        destination: None,
    }
}

#[test]
fn permission_update_to_dict_matches_python() {
    let rules = PermissionUpdate {
        rules: Some(vec![
            PermissionRuleValue {
                tool_name: "Bash".into(),
                rule_content: Some("npm test".into()),
            },
            PermissionRuleValue {
                tool_name: "Read".into(),
                rule_content: None,
            },
        ]),
        behavior: Some("allow".into()),
        // Fields the type does not use are left out.
        mode: Some(PermissionMode::Plan),
        directories: Some(vec!["/x".into()]),
        ..update("addRules")
    };
    assert_eq!(
        rules.to_dict().unwrap(),
        json!({
            "type": "addRules",
            "destination": "session",
            "rules": [{"toolName": "Bash", "ruleContent": "npm test"}, {"toolName": "Read"}],
            "behavior": "allow"
        })
    );

    let mode = PermissionUpdate {
        mode: Some(PermissionMode::AcceptEdits),
        destination: Some("localSettings".into()),
        behavior: Some("deny".into()),
        ..update("setMode")
    };
    assert_eq!(
        mode.to_dict().unwrap(),
        json!({"type": "setMode", "destination": "localSettings", "mode": "acceptEdits"})
    );

    let dirs = PermissionUpdate {
        directories: Some(vec!["/a".into()]),
        ..update("removeDirectories")
    };
    assert_eq!(
        dirs.to_dict().unwrap(),
        json!({"type": "removeDirectories", "destination": "session", "directories": ["/a"]})
    );
}

#[test]
fn permission_update_to_dict_validates_required_fields() {
    let err = |u: PermissionUpdate| u.to_dict().unwrap_err().to_string();

    assert_eq!(
        err(update("replaceRules")),
        "PermissionUpdate(type='replaceRules') has no rules. Set rules to a list of PermissionRuleValue."
    );
    assert_eq!(
        err(PermissionUpdate {
            rules: Some(vec![]),
            ..update("removeRules")
        }),
        "PermissionUpdate(type='removeRules') has no behavior. Set behavior to 'allow', 'deny' or 'ask'."
    );
    assert_eq!(
        err(update("setMode")),
        "PermissionUpdate(type='setMode') has no mode. Set mode to a permission mode such as 'acceptEdits'."
    );
    assert_eq!(
        err(update("addDirectories")),
        "PermissionUpdate(type='addDirectories') has no directories. Set directories to a list of paths."
    );
    assert_eq!(
        err(update("futureType")),
        "PermissionUpdate has type 'futureType', which this version of the SDK cannot send. \
         Use one of addRules, replaceRules, removeRules, setMode, addDirectories or \
         removeDirectories, or upgrade the SDK if this update came from Claude Code."
    );
}

#[test]
fn permission_update_from_dict_is_lenient_inverse() {
    let parsed = PermissionUpdate::from_dict(&json!({
        "type": "addRules",
        "rules": [{"toolName": "Bash", "ruleContent": "ls"}, {"toolName": "Read"}],
        "behavior": "ask"
    }))
    .unwrap();
    assert_eq!(parsed.r#type, "addRules");
    assert!(parsed.destination.is_none());
    assert_eq!(parsed.behavior.as_deref(), Some("ask"));
    let rules = parsed.rules.as_ref().unwrap();
    assert_eq!(rules[1].tool_name, "Read");
    assert!(rules[1].rule_content.is_none());
    assert_eq!(
        parsed.to_dict().unwrap(),
        json!({
            "type": "addRules",
            "destination": "session",
            "rules": [{"toolName": "Bash", "ruleContent": "ls"}, {"toolName": "Read"}],
            "behavior": "ask"
        })
    );

    // Unknown type and behavior are kept verbatim.
    let future = PermissionUpdate::from_dict(&json!({
        "type": "addHosts", "behavior": "maybe", "destination": "cloudSettings"
    }))
    .unwrap();
    assert_eq!(future.r#type, "addHosts");
    assert_eq!(future.behavior.as_deref(), Some("maybe"));
    assert_eq!(future.destination.as_deref(), Some("cloudSettings"));

    assert!(PermissionUpdate::from_dict(&json!({"behavior": "allow"})).is_err());
}

#[test]
fn permission_enums_round_trip_with_other_fallback() {
    assert_eq!(
        serde_json::to_value(PermissionBehavior::Ask).unwrap(),
        json!("ask")
    );
    assert_eq!(
        serde_json::from_value::<PermissionBehavior>(json!("maybe")).unwrap(),
        PermissionBehavior::Other("maybe".into())
    );
    for (dest, wire) in [
        (PermissionUpdateDestination::UserSettings, "userSettings"),
        (
            PermissionUpdateDestination::ProjectSettings,
            "projectSettings",
        ),
        (PermissionUpdateDestination::LocalSettings, "localSettings"),
        (PermissionUpdateDestination::Session, "session"),
    ] {
        assert_eq!(serde_json::to_value(&dest).unwrap(), json!(wire));
        assert_eq!(PermissionUpdateDestination::from(wire), dest);
    }
    for (kind, wire) in [
        (PermissionUpdateType::AddRules, "addRules"),
        (PermissionUpdateType::ReplaceRules, "replaceRules"),
        (PermissionUpdateType::RemoveRules, "removeRules"),
        (PermissionUpdateType::SetMode, "setMode"),
        (PermissionUpdateType::AddDirectories, "addDirectories"),
        (PermissionUpdateType::RemoveDirectories, "removeDirectories"),
    ] {
        assert_eq!(serde_json::to_value(&kind).unwrap(), json!(wire));
        assert_eq!(String::from(kind), wire);
    }

    // The enums plug into the string-typed PermissionUpdate fields.
    let typed = PermissionUpdate {
        behavior: Some(PermissionBehavior::Deny.into()),
        rules: Some(vec![]),
        destination: Some(PermissionUpdateDestination::ProjectSettings.into()),
        ..update(PermissionUpdateType::AddRules.as_str())
    };
    assert_eq!(
        typed.to_dict().unwrap(),
        json!({"type": "addRules", "destination": "projectSettings", "rules": [],
               "behavior": "deny"})
    );
}

// --- can_use_tool shadowing ----------------------------------------------------

fn with_callback(allowed: &[&str]) -> ClaudeAgentOptions {
    ClaudeAgentOptions {
        can_use_tool: Some(allow_all()),
        allowed_tools: allowed.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn can_use_tool_shadowed_warning_matches_python() {
    // No callback: never warns.
    let options = ClaudeAgentOptions {
        allowed_tools: vec!["Read".into()],
        permission_mode: Some(PermissionMode::BypassPermissions),
        ..Default::default()
    };
    assert_eq!(can_use_tool_shadowed_warning(&options), None);

    let bypass = ClaudeAgentOptions {
        permission_mode: Some(PermissionMode::BypassPermissions),
        ..with_callback(&[])
    };
    assert_eq!(
        can_use_tool_shadowed_warning(&bypass),
        Some(CanUseToolShadowedWarning {
            message: "can_use_tool will not be invoked: permission_mode 'bypassPermissions' \
                      auto-approves every tool call (except explicit deny rules) before the \
                      callback is consulted. To gate every tool call, use a PreToolUse hook \
                      instead."
                .into()
        })
    );

    let shadowed = with_callback(&[
        "Read",
        "Read()",
        "Write(*)",
        "Bash(ls:*)",
        "   ",
        "(x)",
        "Edit(",
    ]);
    assert_eq!(
        can_use_tool_shadowed_warning(&shadowed).unwrap().message,
        "can_use_tool will not be invoked for: Read, Write. An allowed_tools entry that allows \
         a whole tool auto-approves it before the callback is consulted. To gate every tool \
         call, use a PreToolUse hook; or narrow the entry so calls fall through to \
         can_use_tool. Allow rules from settings files can also shadow the callback but are \
         not visible here."
    );

    assert_eq!(
        can_use_tool_shadowed_warning(&with_callback(&["Bash(git:*)"])),
        None
    );

    // skills = All implies a bare "Skill" entry.
    let skills = ClaudeAgentOptions {
        skills: Some(SkillsConfig::All),
        ..with_callback(&["Bash(ls)"])
    };
    assert!(can_use_tool_shadowed_warning(&skills)
        .unwrap()
        .message
        .starts_with("can_use_tool will not be invoked for: Skill. "));
    let skills_listed = ClaudeAgentOptions {
        skills: Some(SkillsConfig::All),
        ..with_callback(&["Skill", "Read"])
    };
    assert!(can_use_tool_shadowed_warning(&skills_listed)
        .unwrap()
        .message
        .starts_with("can_use_tool will not be invoked for: Skill, Read. "));
    let skill_names = ClaudeAgentOptions {
        skills: Some(SkillsConfig::Names(vec!["pdf".into()])),
        ..with_callback(&[])
    };
    assert_eq!(can_use_tool_shadowed_warning(&skill_names), None);
}
