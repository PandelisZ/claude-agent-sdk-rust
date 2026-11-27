use claude_code_sdk_rust::sessions::{
    delete_session, fork_session, get_session_info, get_session_messages, get_subagent_messages,
    list_sessions, list_subagents, rename_session, tag_session, ListSessionsOptions,
    SessionMutationOptions, SessionQueryOptions,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::{Mutex, MutexGuard};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn unique_tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "claude-agent-sdk-rust-local-sessions-{name}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

async fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

struct EnvGuard {
    old: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(config: &Path) -> Self {
        let old = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::set_var("CLAUDE_CONFIG_DIR", config);
        Self { old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(old) = &self.old {
            std::env::set_var("CLAUDE_CONFIG_DIR", old);
        } else {
            std::env::remove_var("CLAUDE_CONFIG_DIR");
        }
    }
}

fn write_jsonl(path: &Path, entries: &[serde_json::Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut content = String::new();
    for entry in entries {
        content.push_str(&serde_json::to_string(entry).unwrap());
        content.push('\n');
    }
    std::fs::write(path, content).unwrap();
}

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn query_opts(session_id: &str) -> SessionQueryOptions {
    SessionQueryOptions {
        session_id: session_id.to_string(),
        include_messages: true,
        directory: None,
        limit: None,
        offset: None,
    }
}

fn mutation_opts(session_id: &str) -> SessionMutationOptions {
    SessionMutationOptions {
        session_id: session_id.to_string(),
        directory: None,
    }
}

fn query_opts_for_dir(session_id: &str, directory: &Path) -> SessionQueryOptions {
    SessionQueryOptions {
        directory: Some(directory.to_string_lossy().to_string()),
        ..query_opts(session_id)
    }
}

fn project_dir_for(config: &Path, project: &Path) -> PathBuf {
    config
        .join("projects")
        .join(claude_code_sdk_rust::project_key_for_directory(Some(
            project,
        )))
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[tokio::test]
async fn list_and_info_return_python_sdk_session_info_from_lite_parse() {
    let _lock = env_lock().await;
    let root = unique_tmp("list");
    let config = root.join(".claude");
    let _guard = EnvGuard::set(&config);
    let session_id = new_id();
    let transcript = config
        .join("projects")
        .join("-Users-test-project")
        .join(format!("{session_id}.jsonl"));
    write_jsonl(
        &transcript,
        &[
            json!({"type": "permission-mode", "mode": "default"}),
            json!({
                "type": "user",
                "uuid": "u1",
                "parentUuid": null,
                "sessionId": session_id,
                "cwd": "/Users/test/project",
                "gitBranch": "main",
                "timestamp": "2026-01-01T00:00:00Z",
                "message": {"role": "user", "content": "What is 2+2?"}
            }),
            json!({
                "type": "assistant",
                "uuid": "a1",
                "parentUuid": "u1",
                "sessionId": session_id,
                "gitBranch": "feature",
                "timestamp": "2026-01-01T00:00:01Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "4"}]}
            }),
            json!({"type": "summary", "summary": "Math question"}),
        ],
    );
    let file_size = std::fs::metadata(&transcript).unwrap().len() as i64;

    let sessions = list_sessions(&ListSessionsOptions::default())
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    let info = &sessions[0];
    assert_eq!(info.session_id, session_id);
    assert_eq!(info.summary, "Math question");
    assert_eq!(info.custom_title, None);
    assert_eq!(info.first_prompt.as_deref(), Some("What is 2+2?"));
    assert_eq!(info.git_branch.as_deref(), Some("feature"));
    assert_eq!(info.cwd.as_deref(), Some("/Users/test/project"));
    assert_eq!(info.tag, None);
    assert_eq!(info.created_at, Some(1_767_225_600_000));
    assert_eq!(info.file_size, Some(file_size));
    assert!(info.last_modified > 0);

    let single = get_session_info(&session_id, &query_opts(&session_id))
        .await
        .unwrap()
        .expect("info");
    assert_eq!(&single, info);

    // Python JSON shape of SDKSessionInfo.
    let value = serde_json::to_value(info).unwrap();
    let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "session_id",
            "summary",
            "last_modified",
            "file_size",
            "custom_title",
            "first_prompt",
            "git_branch",
            "cwd",
            "tag",
            "created_at"
        ]
    );
}

#[tokio::test]
async fn get_session_info_returns_none_for_invalid_missing_sidechain_and_empty() {
    let _lock = env_lock().await;
    let root = unique_tmp("info-none");
    let config = root.join(".claude");
    let _guard = EnvGuard::set(&config);
    let project_dir = config.join("projects").join("-proj");
    let sidechain = new_id();
    let empty = new_id();
    let metadata_only = new_id();
    write_jsonl(
        &project_dir.join(format!("{sidechain}.jsonl")),
        &[json!({"type": "user", "uuid": "u", "isSidechain": true, "message": {"content": "hi"}})],
    );
    std::fs::write(project_dir.join(format!("{empty}.jsonl")), "").unwrap();
    write_jsonl(
        &project_dir.join(format!("{metadata_only}.jsonl")),
        &[json!({"type": "permission-mode", "mode": "default"})],
    );

    for id in ["not-a-uuid", "550e8400e29b41d4a716446655440000", &new_id()] {
        assert!(get_session_info(id, &query_opts(id))
            .await
            .unwrap()
            .is_none());
    }
    for id in [&sidechain, &empty, &metadata_only] {
        assert!(get_session_info(id, &query_opts(id))
            .await
            .unwrap()
            .is_none());
    }
    assert!(list_sessions(&ListSessionsOptions::default())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn list_sessions_sorts_pages_and_skips_non_uuid_files() {
    let _lock = env_lock().await;
    let root = unique_tmp("paging");
    let config = root.join(".claude");
    let _guard = EnvGuard::set(&config);
    let project_dir = config.join("projects").join("-proj");
    let mut ids = Vec::new();
    for idx in 0..3 {
        let id = new_id();
        let path = project_dir.join(format!("{id}.jsonl"));
        write_jsonl(
            &path,
            &[
                json!({"type": "user", "uuid": "u", "message": {"content": format!("prompt {idx}")}}),
            ],
        );
        let mtime = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(1_700_000_000 + idx * 10);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        ids.push(id);
    }
    write_jsonl(
        &project_dir.join("not-a-session.jsonl"),
        &[json!({"type": "user", "uuid": "x", "message": {"content": "ignored"}})],
    );

    let all = list_sessions(&ListSessionsOptions::default())
        .await
        .unwrap();
    let listed: Vec<_> = all.iter().map(|info| info.session_id.clone()).collect();
    assert_eq!(listed, vec![ids[2].clone(), ids[1].clone(), ids[0].clone()]);
    assert_eq!(all[0].last_modified, 1_700_000_020_000);

    let page = list_sessions(&ListSessionsOptions {
        limit: Some(1),
        offset: Some(1),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].session_id, ids[1]);
}

#[tokio::test]
async fn messages_follow_parent_chain_and_return_raw_api_message() {
    let _lock = env_lock().await;
    let root = unique_tmp("messages");
    let config = root.join(".claude");
    let _guard = EnvGuard::set(&config);
    let session_id = new_id();
    let transcript = config
        .join("projects")
        .join("-proj")
        .join(format!("{session_id}.jsonl"));
    write_jsonl(
        &transcript,
        &[
            json!({"type": "user", "uuid": "u1", "parentUuid": null, "sessionId": session_id,
                   "message": {"role": "user", "content": "first"}}),
            json!({"type": "assistant", "uuid": "abandoned", "parentUuid": "u1", "sessionId": session_id,
                   "message": {"role": "assistant", "content": "old"}}),
            json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1", "sessionId": session_id,
                   "message": {"role": "assistant", "model": "m", "content": [{"type": "text", "text": "hi"}]}}),
            json!({"type": "user", "uuid": "meta", "parentUuid": "a1", "isMeta": true, "sessionId": session_id,
                   "message": {"role": "user", "content": "hidden"}}),
            json!({"type": "user", "uuid": "team", "parentUuid": "meta", "teamName": "", "sessionId": session_id,
                   "message": {"role": "user", "content": "empty team name is visible"}}),
            json!({"type": "tag", "tag": "x", "sessionId": session_id}),
        ],
    );

    let messages = get_session_messages(&session_id, &query_opts(&session_id))
        .await
        .unwrap();
    let uuids: Vec<_> = messages.iter().map(|m| m.uuid.as_str()).collect();
    assert_eq!(uuids, ["u1", "a1", "team"]);
    assert_eq!(messages[1].r#type, "assistant");
    assert_eq!(messages[1].session_id, session_id);
    assert_eq!(
        serde_json::to_string(&messages[1].message).unwrap(),
        r#"{"role":"assistant","model":"m","content":[{"type":"text","text":"hi"}]}"#
    );
    assert_eq!(messages[1].parent_tool_use_id, None);
    assert_eq!(messages[1].parent_agent_id, None);
    assert_eq!(
        serde_json::to_value(&messages[0]).unwrap(),
        json!({
            "type": "user",
            "uuid": "u1",
            "session_id": session_id,
            "message": {"role": "user", "content": "first"},
            "parent_tool_use_id": null,
            "parent_agent_id": null
        })
    );

    let mut opts = query_opts(&session_id);
    opts.limit = Some(1);
    opts.offset = Some(1);
    let page = get_session_messages(&session_id, &opts).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].uuid, "a1");

    // Invalid UUIDs return empty results instead of errors.
    assert!(get_session_messages("../../etc/passwd", &query_opts("x"))
        .await
        .unwrap()
        .is_empty());
    assert!(list_subagents("nope", &query_opts("x"))
        .await
        .unwrap()
        .is_empty());
}

fn parallel_turn(
    sid: &str,
    parent: &str,
    tools: usize,
    result_order: &[usize],
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let mut calls: Vec<serde_json::Value> = Vec::new();
    for i in 0..tools {
        let parent_uuid = calls
            .last()
            .map(|call| call["uuid"].clone())
            .unwrap_or(json!(parent));
        calls.push(json!({
            "type": "assistant", "uuid": format!("call-{i}"), "parentUuid": parent_uuid,
            "sessionId": sid, "timestamp": format!("2026-01-01T00:00:01.{i:03}Z"),
            "message": {"role": "assistant", "id": "msg_parallel",
                        "content": [{"type": "tool_use", "id": format!("toolu_{i}"), "name": "Read", "input": {}}]}
        }));
    }
    let results = result_order
        .iter()
        .enumerate()
        .map(|(position, i)| {
            json!({
                "type": "user", "uuid": format!("result-{i}"), "parentUuid": format!("call-{i}"),
                "sessionId": sid, "sourceToolAssistantUUID": format!("call-{i}"),
                "timestamp": format!("2026-01-01T00:00:02.{position:03}Z"),
                "message": {"role": "user",
                            "content": [{"type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": "ok"}]}
            })
        })
        .collect();
    (calls, results)
}

#[tokio::test]
async fn messages_keep_every_parallel_tool_call_and_result() {
    let _lock = env_lock().await;
    let root = unique_tmp("parallel");
    let config = root.join(".claude");
    let project = root.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let _guard = EnvGuard::set(&config);
    for order in [[0usize, 1, 2], [2, 1, 0], [1, 2, 0], [0, 2, 1]] {
        let sid = new_id();
        let (calls, results) = parallel_turn(&sid, "prompt", 3, &order);
        let mut entries = vec![json!({"type": "user", "uuid": "prompt", "parentUuid": null,
                                      "sessionId": sid, "message": {"role": "user", "content": "read them"}})];
        entries.extend(calls);
        let last_result = results.last().unwrap()["uuid"].clone();
        entries.extend(results);
        entries.push(
            json!({"type": "assistant", "uuid": "answer", "parentUuid": last_result,
                            "sessionId": sid, "message": {"role": "assistant", "content": "done"}}),
        );
        write_jsonl(
            &project_dir_for(&config, &project).join(format!("{sid}.jsonl")),
            &entries,
        );

        let messages = get_session_messages(&sid, &query_opts_for_dir(&sid, &project))
            .await
            .unwrap();
        assert_eq!(messages.len(), 8, "order {order:?}");
        assert_eq!(messages[0].uuid, "prompt");
        let types: Vec<_> = messages[1..7].iter().map(|m| m.r#type.as_str()).collect();
        assert_eq!(
            types,
            [
                "assistant",
                "assistant",
                "assistant",
                "user",
                "user",
                "user"
            ]
        );
        assert_eq!(messages[7].uuid, "answer");
    }

    // Last-turn recovery keeps completion order.
    let sid = new_id();
    let (calls, results) = parallel_turn(&sid, "prompt", 2, &[1, 0]);
    let mut entries = vec![json!({"type": "user", "uuid": "prompt", "parentUuid": null,
                                  "sessionId": sid, "message": {"role": "user", "content": "go"}})];
    entries.extend(calls);
    entries.extend(results);
    write_jsonl(
        &project_dir_for(&config, &project).join(format!("{sid}.jsonl")),
        &entries,
    );
    let messages = get_session_messages(&sid, &query_opts_for_dir(&sid, &project))
        .await
        .unwrap();
    let uuids: Vec<_> = messages.iter().map(|m| m.uuid.as_str()).collect();
    assert_eq!(
        uuids,
        ["prompt", "call-0", "call-1", "result-1", "result-0"]
    );
}

#[tokio::test]
async fn rename_appends_python_custom_title_line_and_skips_empty_stubs() {
    let _lock = env_lock().await;
    let root = unique_tmp("rename");
    let config = root.join(".claude");
    let _guard = EnvGuard::set(&config);
    let session_id = new_id();
    let real = config
        .join("projects")
        .join("-real")
        .join(format!("{session_id}.jsonl"));
    let stub = config
        .join("projects")
        .join("-stub")
        .join(format!("{session_id}.jsonl"));
    write_jsonl(
        &real,
        &[json!({"type": "user", "uuid": "u", "message": {"content": "Visible prompt"}})],
    );
    std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
    std::fs::write(&stub, "").unwrap();

    rename_session(
        &session_id,
        "  Caf\u{e9} title  ",
        &mutation_opts(&session_id),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(&real).unwrap();
    let last = content.lines().last().unwrap();
    assert_eq!(
        last,
        format!(
            r#"{{"type":"custom-title","customTitle":"Caf\u00e9 title","sessionId":"{session_id}"}}"#
        )
    );
    assert_eq!(std::fs::metadata(&stub).unwrap().len(), 0);

    let info = get_session_info(&session_id, &query_opts(&session_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.custom_title.as_deref(), Some("Caf\u{e9} title"));
    assert_eq!(info.summary, "Caf\u{e9} title");

    let err = rename_session(&session_id, "   ", &mutation_opts(&session_id))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("title must be non-empty"));
    let err = rename_session("bad", "x", &mutation_opts("bad"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Invalid session_id: bad"));

    let missing = new_id();
    let err = rename_session(&missing, "x", &mutation_opts(&missing))
        .await
        .unwrap_err();
    assert!(err.to_string().contains(&format!(
        "Session {missing} not found in any project directory"
    )));
    let project = root.join("p");
    std::fs::create_dir_all(&project).unwrap();
    let err = rename_session(
        &missing,
        "x",
        &SessionMutationOptions {
            session_id: missing.clone(),
            directory: Some(project.to_string_lossy().to_string()),
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains(&format!(
        "Session {missing} not found in project directory for {}",
        project.display()
    )));
}

#[tokio::test]
async fn tag_lines_start_with_type_and_are_visible_to_lite_parse() {
    let _lock = env_lock().await;
    let root = unique_tmp("tag");
    let config = root.join(".claude");
    let _guard = EnvGuard::set(&config);
    let session_id = new_id();
    let transcript = config
        .join("projects")
        .join("-Users-test-project")
        .join(format!("{session_id}.jsonl"));
    write_jsonl(
        &transcript,
        &[json!({"type": "user", "uuid": "visible", "message": {"content": "Visible prompt"}})],
    );

    tag_session(
        &session_id,
        Some("  exp\u{200b}eri\u{202e}ment\u{feff}  "),
        &mutation_opts(&session_id),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(&transcript).unwrap();
    assert_eq!(
        content.lines().nth(1).unwrap(),
        format!(r#"{{"type":"tag","tag":"experiment","sessionId":"{session_id}"}}"#)
    );
    let info = get_session_info(&session_id, &query_opts(&session_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.tag.as_deref(), Some("experiment"));

    tag_session(&session_id, None, &mutation_opts(&session_id))
        .await
        .unwrap();
    let lines = read_lines(&transcript);
    assert_eq!(
        lines[2],
        json!({"type": "tag", "tag": "", "sessionId": session_id})
    );
    let info = get_session_info(&session_id, &query_opts(&session_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.tag, None);

    let err = tag_session(
        &session_id,
        Some(" \u{200b}\u{200d} "),
        &mutation_opts(&session_id),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("tag must be non-empty"));
}

#[tokio::test]
async fn delete_removes_transcript_and_errors_when_missing() {
    let _lock = env_lock().await;
    let root = unique_tmp("delete");
    let config = root.join(".claude");
    let _guard = EnvGuard::set(&config);
    let session_id = new_id();
    let project_dir = config.join("projects").join("-Users-test-project");
    let transcript = project_dir.join(format!("{session_id}.jsonl"));
    let sidecar_dir = project_dir.join(&session_id);
    std::fs::create_dir_all(sidecar_dir.join("subagents")).unwrap();
    std::fs::write(sidecar_dir.join("subagents").join("agent-1.jsonl"), "{}\n").unwrap();
    write_jsonl(
        &transcript,
        &[json!({"type": "user", "uuid": "u", "message": {"content": "x"}})],
    );

    delete_session(&session_id, &mutation_opts(&session_id))
        .await
        .unwrap();
    assert!(!transcript.exists());
    assert!(!sidecar_dir.exists());

    let err = delete_session(&session_id, &mutation_opts(&session_id))
        .await
        .unwrap_err();
    assert!(err
        .to_string()
        .contains(&format!("Session {session_id} not found")));

    // A 0-byte stub is not a session.
    std::fs::write(&transcript, "").unwrap();
    assert!(delete_session(&session_id, &mutation_opts(&session_id))
        .await
        .is_err());
    assert!(transcript.exists());
}

#[tokio::test]
async fn subagent_messages_keep_sidechain_lines_and_read_meta_sidecar() {
    let _lock = env_lock().await;
    let root = unique_tmp("subagents");
    let config = root.join(".claude");
    let _guard = EnvGuard::set(&config);
    let session_id = new_id();
    let project_dir = config.join("projects").join("-Users-test-project");
    write_jsonl(
        &project_dir.join(format!("{session_id}.jsonl")),
        &[json!({"type": "user", "uuid": "root", "message": {"content": "parent prompt"}})],
    );
    let subagent_file = project_dir
        .join(&session_id)
        .join("subagents")
        .join("workflows")
        .join("run-1")
        .join("agent-deep.jsonl");
    // Real CLI subagent transcripts set isSidechain:true on every line.
    write_jsonl(
        &subagent_file,
        &[
            json!({"type": "assistant", "uuid": "branch", "parentUuid": "missing", "isSidechain": true,
                   "message": {"content": "old branch"}}),
            json!({"type": "user", "uuid": "s1", "isSidechain": true, "sessionId": session_id,
                   "message": {"content": "sub prompt"}}),
            json!({"type": "progress", "uuid": "p1", "parentUuid": "s1", "isSidechain": true}),
            json!({"type": "assistant", "uuid": "s2", "parentUuid": "p1", "isSidechain": true,
                   "message": {"content": [{"type": "text", "text": "sub answer"}]}}),
            json!({"type": "agent-name", "agentName": "x"}),
        ],
    );
    std::fs::write(
        subagent_file.with_file_name("agent-deep.meta.json"),
        r#"{"agentType":"general","toolUseId":"toolu_parent","parentAgentId":"agent-outer"}"#,
    )
    .unwrap();

    let opts = query_opts(&session_id);
    assert_eq!(
        list_subagents(&session_id, &opts).await.unwrap(),
        vec!["deep".to_string()]
    );

    let messages = get_subagent_messages(&session_id, "deep", &opts)
        .await
        .unwrap();
    let uuids: Vec<_> = messages.iter().map(|m| m.uuid.as_str()).collect();
    assert_eq!(uuids, ["s1", "s2"]);
    assert_eq!(messages[0].session_id, session_id);
    assert_eq!(messages[1].message["content"][0]["text"], "sub answer");
    for message in &messages {
        assert_eq!(message.parent_tool_use_id.as_deref(), Some("toolu_parent"));
        assert_eq!(message.parent_agent_id.as_deref(), Some("agent-outer"));
    }

    // Corrupt sidecar degrades to no metadata.
    std::fs::write(
        subagent_file.with_file_name("agent-deep.meta.json"),
        "{not json",
    )
    .unwrap();
    let messages = get_subagent_messages(&session_id, "deep", &opts)
        .await
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].parent_tool_use_id, None);

    assert!(get_subagent_messages(&session_id, "missing", &opts)
        .await
        .unwrap()
        .is_empty());
    assert!(get_subagent_messages(&session_id, "", &opts)
        .await
        .unwrap()
        .is_empty());
    assert!(get_subagent_messages("bad-id", "deep", &opts)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn directory_scoping_and_long_path_prefix_fallback() {
    let _lock = env_lock().await;
    let root = unique_tmp("directory");
    let config = root.join(".claude");
    let project_a = root.join("project-a");
    let project_b = root.join("project-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    let _guard = EnvGuard::set(&config);
    let session_a = new_id();
    let session_b = new_id();
    write_jsonl(
        &project_dir_for(&config, &project_a).join(format!("{session_a}.jsonl")),
        &[json!({"type": "user", "uuid": "a1", "message": {"content": "a one"}})],
    );
    write_jsonl(
        &project_dir_for(&config, &project_b).join(format!("{session_b}.jsonl")),
        &[json!({"type": "user", "uuid": "b1", "message": {"content": "b one"}})],
    );

    let sessions = list_sessions(&ListSessionsOptions {
        directory: Some(project_a.to_string_lossy().to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, session_a);
    assert!(
        get_session_messages(&session_b, &query_opts_for_dir(&session_b, &project_a))
            .await
            .unwrap()
            .is_empty()
    );

    // A >200-char project path whose on-disk dir carries a different hash
    // suffix (Bun.hash vs simpleHash) is still found by prefix.
    let long = root.join("x".repeat(240));
    let key = claude_code_sdk_rust::project_key_for_directory(Some(&long));
    assert!(key.len() > 200);
    let on_disk = config
        .join("projects")
        .join(format!("{}-otherhash", &key[..200]));
    let long_session = new_id();
    write_jsonl(
        &on_disk.join(format!("{long_session}.jsonl")),
        &[json!({"type": "user", "uuid": "l1", "message": {"content": "long"}})],
    );
    let sessions = list_sessions(&ListSessionsOptions {
        directory: Some(long.to_string_lossy().to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, long_session);
    assert_eq!(sessions[0].cwd.as_deref(), Some(long.to_str().unwrap()));
}

fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test]
async fn git_worktree_sessions_are_listed_and_resolved() {
    let _lock = env_lock().await;
    let root = unique_tmp("worktrees");
    let config = root.join(".claude");
    let repo = root.join("repo");
    let worktree = root.join("repo-wt");
    std::fs::create_dir_all(&repo).unwrap();
    if !(git(&repo, &["init", "-q"])
        && git(
            &repo,
            &[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        )
        && git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                worktree.to_str().unwrap(),
                "-b",
                "wt",
            ],
        ))
    {
        eprintln!("git unavailable; skipping worktree test");
        return;
    }
    let _guard = EnvGuard::set(&config);
    let main_session = new_id();
    let wt_session = new_id();
    write_jsonl(
        &project_dir_for(&config, &repo).join(format!("{main_session}.jsonl")),
        &[json!({"type": "user", "uuid": "m", "message": {"content": "main"}})],
    );
    write_jsonl(
        &project_dir_for(&config, &worktree).join(format!("{wt_session}.jsonl")),
        &[json!({"type": "user", "uuid": "w", "message": {"content": "worktree"}})],
    );

    let directory = Some(repo.to_string_lossy().to_string());
    let mut ids: Vec<_> = list_sessions(&ListSessionsOptions {
        directory: directory.clone(),
        ..Default::default()
    })
    .await
    .unwrap()
    .into_iter()
    .map(|info| info.session_id)
    .collect();
    ids.sort();
    let mut expected = vec![main_session.clone(), wt_session.clone()];
    expected.sort();
    assert_eq!(ids, expected);

    let only_main = list_sessions(&ListSessionsOptions {
        directory: directory.clone(),
        include_worktrees: false,
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(only_main.len(), 1);
    assert_eq!(only_main[0].session_id, main_session);

    // Lookups with the main checkout as directory fall back to worktrees.
    let messages = get_session_messages(&wt_session, &query_opts_for_dir(&wt_session, &repo))
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
    let info = get_session_info(&wt_session, &query_opts_for_dir(&wt_session, &repo))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.cwd.as_deref(), Some(worktree.to_str().unwrap()));
    tag_session(
        &wt_session,
        Some("wt"),
        &SessionMutationOptions {
            session_id: wt_session.clone(),
            directory: directory.clone(),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn fork_matches_python_build_fork_lines() {
    let _lock = env_lock().await;
    let root = unique_tmp("fork");
    let config = root.join(".claude");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let _guard = EnvGuard::set(&config);
    let session_id = new_id();
    let u1 = new_id();
    let p1 = new_id();
    let a1 = new_id();
    let side = new_id();
    let u2 = new_id();
    let project_dir = project_dir_for(&config, &project);
    write_jsonl(
        &project_dir.join(format!("{session_id}.jsonl")),
        &[
            json!({"type": "permission-mode", "mode": "default", "sessionId": session_id}),
            json!({"type": "user", "uuid": u1, "parentUuid": null, "sessionId": session_id,
                   "timestamp": "2026-01-01T00:00:00Z", "teamName": "t", "slug": "s",
                   "message": {"content": "Original prompt"}}),
            json!({"type": "progress", "uuid": p1, "parentUuid": u1, "sessionId": session_id}),
            json!({"type": "assistant", "uuid": a1, "parentUuid": p1, "sessionId": session_id,
                   "timestamp": "2026-01-01T00:00:01Z", "logicalParentUuid": u1,
                   "sourceToolAssistantUUID": "x", "message": {"content": "two"}}),
            json!({"type": "user", "uuid": side, "parentUuid": a1, "isSidechain": true,
                   "sessionId": session_id, "message": {"content": "side"}}),
            json!({"type": "content-replacement", "sessionId": session_id,
                   "replacements": [{"kind": "r1"}]}),
            json!({"type": "content-replacement", "sessionId": "other", "replacements": [{"kind": "skip"}]}),
            json!({"type": "user", "uuid": u2, "parentUuid": a1, "sessionId": session_id,
                   "message": {"content": "three"}}),
        ],
    );
    let opts = SessionMutationOptions {
        session_id: session_id.clone(),
        directory: Some(project.to_string_lossy().to_string()),
    };

    let fork = fork_session(&session_id, &opts, Some(&a1), None)
        .await
        .unwrap();
    assert_ne!(fork.session_id, session_id);
    let fork_path = project_dir.join(format!("{}.jsonl", fork.session_id));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&fork_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let entries = read_lines(&fork_path);
    // user + assistant (progress dropped), content-replacement, custom-title.
    assert_eq!(entries.len(), 4);
    let user = entries[0].as_object().unwrap();
    let keys: Vec<_> = user.keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "type",
            "uuid",
            "parentUuid",
            "sessionId",
            "timestamp",
            "message",
            "logicalParentUuid",
            "isSidechain",
            "forkedFrom"
        ]
    );
    assert_eq!(user["sessionId"], fork.session_id);
    assert_eq!(
        user["forkedFrom"],
        json!({"sessionId": session_id, "messageUuid": u1})
    );
    assert_eq!(user["parentUuid"], serde_json::Value::Null);
    assert_eq!(user["logicalParentUuid"], serde_json::Value::Null);
    assert_eq!(user["timestamp"], "2026-01-01T00:00:00Z");
    assert_eq!(user["isSidechain"], false);
    assert!(user.get("teamName").is_none() && user.get("slug").is_none());

    let assistant = entries[1].as_object().unwrap();
    // Parent skips the progress entry and points at the remapped user entry.
    assert_eq!(assistant["parentUuid"], user["uuid"]);
    assert_eq!(assistant["logicalParentUuid"], user["uuid"]);
    assert!(assistant.get("sourceToolAssistantUUID").is_none());
    let ts = assistant["timestamp"].as_str().unwrap();
    assert!(ts.ends_with('Z') && !ts.contains("+00:00") && ts != "2026-01-01T00:00:01Z");

    assert_eq!(entries[2]["type"], "content-replacement");
    assert_eq!(entries[2]["replacements"], json!([{"kind": "r1"}]));
    assert_eq!(entries[2]["sessionId"], fork.session_id);
    assert_eq!(entries[3]["type"], "custom-title");
    assert_eq!(entries[3]["customTitle"], "Original prompt (fork)");
    assert_eq!(
        entries[3]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["type", "sessionId", "customTitle", "uuid", "timestamp"]
    );

    let titled = fork_session(&session_id, &opts, None, Some("  Fork title  "))
        .await
        .unwrap();
    let entries = read_lines(&project_dir.join(format!("{}.jsonl", titled.session_id)));
    assert_eq!(entries.last().unwrap()["customTitle"], "Fork title");
    assert!(!entries
        .iter()
        .any(|entry| entry["forkedFrom"]["messageUuid"] == side));

    let missing = new_id();
    let err = fork_session(&session_id, &opts, Some(&missing), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains(&format!(
        "Message {missing} not found in session {session_id}"
    )));
    let err = fork_session(&session_id, &opts, Some("not-a-uuid"), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Invalid up_to_message_id"));
    let err = fork_session(&missing, &opts, None, None).await.unwrap_err();
    assert!(err.to_string().contains(&format!(
        "Session {missing} not found in project directory for {}",
        project.display()
    )));
}
