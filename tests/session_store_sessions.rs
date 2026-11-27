use async_trait::async_trait;
use claude_code_sdk_rust::session_store::{
    project_key_for_directory, InMemorySessionStore, SessionKey, SessionListSubkeysKey,
    SessionStore, SessionStoreEntry, SessionStoreHandle, SessionStoreListEntry,
    SessionSummaryEntry,
};
use claude_code_sdk_rust::sessions::{
    delete_session_via_store, fork_session_via_store, get_session_info_from_store,
    get_session_messages_from_store, get_subagent_messages_from_store, list_sessions_from_store,
    list_subagents_from_store, rename_session_via_store, tag_session_via_store,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn obj(value: serde_json::Value) -> SessionStoreEntry {
    value.as_object().unwrap().clone()
}

fn entry(
    message_type: &str,
    session_id: &str,
    uuid: &str,
    parent: Option<&str>,
    content: &str,
) -> SessionStoreEntry {
    obj(json!({
        "type": message_type,
        "uuid": uuid,
        "parentUuid": parent,
        "timestamp": "2026-05-08T10:00:00Z",
        "sessionId": session_id,
        "cwd": "/repo",
        "message": {
            "role": message_type,
            "content": content
        }
    }))
}

fn main_key(project_key: &str, session_id: &str) -> SessionKey {
    SessionKey {
        project_key: project_key.to_string(),
        session_id: session_id.to_string(),
        subpath: None,
    }
}

fn temp_cwd(name: &str) -> std::path::PathBuf {
    let cwd =
        std::env::temp_dir().join(format!("claude-rust-store-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&cwd).unwrap();
    cwd
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[tokio::test]
async fn lists_sessions_from_store_using_summary_sidecars() {
    let cwd = temp_cwd("list");
    let project_key = project_key_for_directory(Some(&cwd));
    let store = InMemorySessionStore::new();
    let older = new_id();
    let newer = new_id();

    store
        .append(
            main_key(&project_key, &older),
            vec![entry("user", &older, "u1", None, "older prompt")],
        )
        .await
        .unwrap();
    store
        .append(
            main_key(&project_key, &newer),
            vec![entry("user", &newer, "u2", None, "newer prompt")],
        )
        .await
        .unwrap();
    let handle = SessionStoreHandle::new(store);

    let sessions = list_sessions_from_store(&handle, Some(cwd.to_str().unwrap()), None, 0)
        .await
        .unwrap();

    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].session_id, newer);
    assert_eq!(sessions[0].summary, "newer prompt");
    assert_eq!(sessions[0].file_size, None);
    assert_eq!(sessions[1].session_id, older);

    let page = list_sessions_from_store(&handle, Some(cwd.to_str().unwrap()), Some(1), 1)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].session_id, older);

    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn gets_lite_parsed_info_and_chain_messages_from_store() {
    let cwd = temp_cwd("messages");
    let project_key = project_key_for_directory(Some(&cwd));
    let session_id = new_id();
    let store = InMemorySessionStore::new();
    let entries = vec![
        entry("user", &session_id, "u1", None, "hello"),
        entry("assistant", &session_id, "a1", Some("u1"), "hi"),
        obj(
            json!({"type": "user", "uuid": "m1", "parentUuid": "a1", "isMeta": true,
                   "sessionId": session_id, "message": {"content": "meta"}}),
        ),
        // Postgres JSONB-style key reordering: `type` is not first.
        obj(json!({"tag": "important", "sessionId": session_id, "type": "tag"})),
        obj(
            json!({"type": "custom-title", "customTitle": "x", "timestamp": "2026-05-08T11:00:00Z"}),
        ),
    ];
    store
        .append(main_key(&project_key, &session_id), entries)
        .await
        .unwrap();
    let handle = SessionStoreHandle::new(store);

    let info = get_session_info_from_store(&handle, &session_id, Some(cwd.to_str().unwrap()))
        .await
        .unwrap()
        .expect("info");
    assert_eq!(info.summary, "x");
    assert_eq!(info.first_prompt.as_deref(), Some("hello"));
    assert_eq!(info.tag.as_deref(), Some("important"));
    assert_eq!(info.cwd.as_deref(), Some("/repo"));
    // mtime comes from the last entry's timestamp; file_size from the JSONL.
    assert_eq!(info.last_modified, 1_778_238_000_000);
    assert!(info.file_size.unwrap() > 0);

    let messages =
        get_session_messages_from_store(&handle, &session_id, Some(cwd.to_str().unwrap()), None, 0)
            .await
            .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].r#type, "user");
    assert_eq!(messages[0].session_id, session_id);
    assert_eq!(messages[1].r#type, "assistant");
    assert_eq!(messages[1].parent_tool_use_id, None);

    let messages = get_session_messages_from_store(
        &handle,
        &session_id,
        Some(cwd.to_str().unwrap()),
        Some(1),
        1,
    )
    .await
    .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].uuid, "a1");

    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn invalid_store_session_id_returns_empty_results() {
    let handle = SessionStoreHandle::new(InMemorySessionStore::new());

    assert!(
        get_session_info_from_store(&handle, "../../etc/passwd", None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        get_session_messages_from_store(&handle, "../../etc/passwd", None, None, 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(list_subagents_from_store(&handle, "nope", None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn mutates_store_session_metadata_and_delete() {
    let cwd = temp_cwd("mutate");
    let project_key = project_key_for_directory(Some(&cwd));
    let session_id = new_id();
    let store = InMemorySessionStore::new();
    store
        .append(
            main_key(&project_key, &session_id),
            vec![entry("user", &session_id, "u1", None, "original")],
        )
        .await
        .unwrap();
    let handle = SessionStoreHandle::new(store.clone());
    let dir = Some(cwd.to_str().unwrap());

    rename_session_via_store(&handle, &session_id, "  renamed  ", dir)
        .await
        .unwrap();
    tag_session_via_store(&handle, &session_id, Some(" impor\u{200b}tant "), dir)
        .await
        .unwrap();

    let stored = store.get_entries(main_key(&project_key, &session_id)).await;
    let rename = &stored[1];
    assert_eq!(
        rename.keys().cloned().collect::<Vec<_>>(),
        ["type", "customTitle", "sessionId", "uuid", "timestamp"]
    );
    assert_eq!(rename["type"], "custom-title");
    assert_eq!(rename["customTitle"], "renamed");
    let timestamp = rename["timestamp"].as_str().unwrap();
    assert!(timestamp.ends_with('Z') && !timestamp.contains("+00:00"));
    let tag = &stored[2];
    assert_eq!(
        tag.keys().cloned().collect::<Vec<_>>(),
        ["type", "tag", "sessionId", "uuid", "timestamp"]
    );
    assert_eq!(tag["tag"], "important");

    let info = get_session_info_from_store(&handle, &session_id, dir)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.summary, "renamed");
    assert_eq!(info.custom_title.as_deref(), Some("renamed"));
    assert_eq!(info.tag.as_deref(), Some("important"));

    tag_session_via_store(&handle, &session_id, None, dir)
        .await
        .unwrap();
    let info = get_session_info_from_store(&handle, &session_id, dir)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.tag, None);

    delete_session_via_store(&handle, &session_id, dir)
        .await
        .unwrap();
    assert!(get_session_info_from_store(&handle, &session_id, dir)
        .await
        .unwrap()
        .is_none());

    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn lists_and_reads_subagent_messages_with_agent_metadata_ids() {
    let cwd = temp_cwd("subagents");
    let project_key = project_key_for_directory(Some(&cwd));
    let session_id = new_id();
    let store = InMemorySessionStore::new();
    store
        .append(
            SessionKey {
                project_key: project_key.clone(),
                session_id: session_id.clone(),
                subpath: Some("subagents/workflows/run-1/agent-alpha".to_string()),
            },
            vec![
                obj(json!({"type": "user", "uuid": "s1", "isSidechain": true, "sessionId": session_id,
                           "message": {"content": "sub hello"}})),
                obj(json!({"type": "assistant", "uuid": "s2", "parentUuid": "s1", "isSidechain": true,
                           "sessionId": session_id, "message": {"content": "sub answer"}})),
                obj(json!({"type": "agent_metadata", "toolUseId": "toolu_old"})),
                obj(json!({"type": "agent_metadata", "toolUseId": "toolu_new", "parentAgentId": "outer"})),
            ],
        )
        .await
        .unwrap();
    store
        .append(
            SessionKey {
                project_key: project_key.clone(),
                session_id: session_id.clone(),
                subpath: Some("metadata/ignored".to_string()),
            },
            vec![entry("user", &session_id, "x", None, "ignored")],
        )
        .await
        .unwrap();
    let handle = SessionStoreHandle::new(store);
    let dir = Some(cwd.to_str().unwrap());

    let subagents = list_subagents_from_store(&handle, &session_id, dir)
        .await
        .unwrap();
    assert_eq!(subagents, vec!["alpha".to_string()]);

    let messages = get_subagent_messages_from_store(&handle, &session_id, "alpha", dir, None, 0)
        .await
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].message["content"], "sub hello");
    for message in &messages {
        assert_eq!(message.parent_tool_use_id.as_deref(), Some("toolu_new"));
        assert_eq!(message.parent_agent_id.as_deref(), Some("outer"));
    }

    // list_subkeys is supported but the agent is absent -> [] (no direct path).
    assert!(
        get_subagent_messages_from_store(&handle, &session_id, "missing", dir, None, 0)
            .await
            .unwrap()
            .is_empty()
    );

    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn forks_store_session_like_build_fork_lines() {
    let cwd = temp_cwd("fork");
    let project_key = project_key_for_directory(Some(&cwd));
    let session_id = new_id();
    let store = InMemorySessionStore::new();
    let first = new_id();
    let second = new_id();
    store
        .append(
            main_key(&project_key, &session_id),
            vec![
                entry("user", &session_id, &first, None, "first"),
                entry("assistant", &session_id, &second, Some(&first), "second"),
                obj(json!({"type": "ai-title", "aiTitle": "AI title", "sessionId": session_id})),
            ],
        )
        .await
        .unwrap();
    let handle = SessionStoreHandle::new(store.clone());
    let dir = Some(cwd.to_str().unwrap());

    let fork = fork_session_via_store(&handle, &session_id, dir, None, None)
        .await
        .unwrap();
    assert_ne!(fork.session_id, session_id);
    let forked = store
        .get_entries(main_key(&project_key, &fork.session_id))
        .await;
    // The ai-title metadata line is not copied; a custom-title is appended.
    assert_eq!(forked.len(), 3);
    assert_eq!(
        forked[0]["forkedFrom"],
        json!({"sessionId": session_id, "messageUuid": first})
    );
    assert_eq!(forked[1]["parentUuid"], forked[0]["uuid"]);
    assert_eq!(forked[2]["type"], "custom-title");
    assert_eq!(forked[2]["customTitle"], "AI title (fork)");

    let fork_messages = get_session_messages_from_store(&handle, &fork.session_id, dir, None, 0)
        .await
        .unwrap();
    assert_eq!(fork_messages.len(), 2);
    assert_ne!(fork_messages[0].uuid, first);
    assert_eq!(fork_messages[0].session_id, fork.session_id);

    let titled = fork_session_via_store(&handle, &session_id, dir, None, Some("fork title"))
        .await
        .unwrap();
    let info = get_session_info_from_store(&handle, &titled.session_id, dir)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.custom_title.as_deref(), Some("fork title"));

    // Missing (None) and emptied (Some([])) sessions are both "not found".
    let missing = new_id();
    let err = fork_session_via_store(&handle, &missing, dir, None, None)
        .await
        .unwrap_err();
    assert!(err
        .to_string()
        .contains(&format!("Session {missing} not found")));

    let _ = tokio::fs::remove_dir_all(cwd).await;
}

/// A store with configurable optional-method support and call counters.
#[derive(Clone, Default)]
struct ProbeStore {
    inner: InMemorySessionStore,
    summaries: Option<Vec<SessionSummaryEntry>>,
    listing: Option<Vec<SessionStoreListEntry>>,
    loads: Arc<AtomicUsize>,
}

#[async_trait]
impl SessionStore for ProbeStore {
    fn supports_list_sessions(&self) -> bool {
        self.listing.is_some()
    }

    fn supports_list_session_summaries(&self) -> bool {
        self.summaries.is_some()
    }

    async fn append(
        &self,
        key: SessionKey,
        entries: Vec<SessionStoreEntry>,
    ) -> claude_code_sdk_rust::Result<()> {
        self.inner.append(key, entries).await
    }

    async fn load(
        &self,
        key: SessionKey,
    ) -> claude_code_sdk_rust::Result<Option<Vec<SessionStoreEntry>>> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.inner.load(key).await
    }

    async fn list_sessions(
        &self,
        _project_key: &str,
    ) -> claude_code_sdk_rust::Result<Vec<SessionStoreListEntry>> {
        Ok(self.listing.clone().unwrap_or_default())
    }

    async fn list_session_summaries(
        &self,
        _project_key: &str,
    ) -> claude_code_sdk_rust::Result<Vec<SessionSummaryEntry>> {
        Ok(self.summaries.clone().unwrap_or_default())
    }
}

fn summary(session_id: &str, mtime: i64, prompt: &str) -> SessionSummaryEntry {
    SessionSummaryEntry {
        session_id: session_id.to_string(),
        mtime,
        data: obj(json!({"first_prompt": prompt, "first_prompt_locked": true})),
    }
}

#[tokio::test]
async fn summary_fast_path_gap_fills_after_pagination_and_drops_unlisted() {
    let cwd = temp_cwd("fast");
    let project_key = project_key_for_directory(Some(&cwd));
    let fresh = new_id();
    let stale = new_id();
    let missing_summary = new_id();
    let unlisted = new_id();
    let inner = InMemorySessionStore::new();
    for (id, prompt) in [(&stale, "stale reloaded"), (&missing_summary, "gap filled")] {
        inner
            .append(
                main_key(&project_key, id),
                vec![entry("user", id, "u", None, prompt)],
            )
            .await
            .unwrap();
    }
    let store = ProbeStore {
        inner,
        summaries: Some(vec![
            summary(&fresh, 300, "fresh summary"),
            summary(&stale, 100, "stale summary"),
            summary(&unlisted, 999, "unlisted"),
        ]),
        listing: Some(vec![
            SessionStoreListEntry {
                session_id: fresh.clone(),
                mtime: 300,
            },
            SessionStoreListEntry {
                session_id: stale.clone(),
                mtime: 200,
            },
            SessionStoreListEntry {
                session_id: missing_summary.clone(),
                mtime: 100,
            },
        ]),
        ..Default::default()
    };
    let loads = store.loads.clone();
    let handle = SessionStoreHandle::new(store);
    let dir = Some(cwd.to_str().unwrap());

    let all = list_sessions_from_store(&handle, dir, None, 0)
        .await
        .unwrap();
    let rows: Vec<_> = all
        .iter()
        .map(|info| {
            (
                info.session_id.clone(),
                info.summary.clone(),
                info.last_modified,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (fresh.clone(), "fresh summary".to_string(), 300),
            (stale.clone(), "stale reloaded".to_string(), 200),
            (missing_summary.clone(), "gap filled".to_string(), 100),
        ]
    );
    assert_eq!(loads.load(Ordering::SeqCst), 2);

    // Only the page's gap slots are loaded.
    loads.store(0, Ordering::SeqCst);
    let page = list_sessions_from_store(&handle, dir, Some(1), 0)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].session_id, fresh);
    assert_eq!(loads.load(Ordering::SeqCst), 0);

    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn summaries_only_store_lists_without_list_sessions() {
    let id = new_id();
    let store = ProbeStore {
        summaries: Some(vec![summary(&id, 5, "only summary")]),
        ..Default::default()
    };
    let handle = SessionStoreHandle::new(store);
    let sessions = list_sessions_from_store(&handle, None, None, 0)
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].summary, "only summary");
}

#[tokio::test]
async fn subagent_listing_requires_list_subkeys_support() {
    let handle = SessionStoreHandle::new(ProbeStore::default());
    let err = list_subagents_from_store(&handle, &new_id(), None)
        .await
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("session_store does not implement list_subkeys()"));

    // Without list_subkeys the direct subagents/agent-<id> path is tried.
    let cwd = temp_cwd("direct");
    let project_key = project_key_for_directory(Some(&cwd));
    let session_id = new_id();
    let store = ProbeStore::default();
    store
        .append(
            SessionKey {
                project_key: project_key.clone(),
                session_id: session_id.clone(),
                subpath: Some("subagents/agent-a".to_string()),
            },
            vec![entry("user", &session_id, "s1", None, "direct")],
        )
        .await
        .unwrap();
    assert!(!store
        .inner
        .list_subkeys(SessionListSubkeysKey {
            project_key,
            session_id: session_id.clone(),
        })
        .await
        .unwrap()
        .is_empty());
    let handle = SessionStoreHandle::new(store);
    let messages = get_subagent_messages_from_store(
        &handle,
        &session_id,
        "a",
        Some(cwd.to_str().unwrap()),
        None,
        0,
    )
    .await
    .unwrap();
    assert_eq!(messages.len(), 1);
    let _ = tokio::fs::remove_dir_all(cwd).await;
}
