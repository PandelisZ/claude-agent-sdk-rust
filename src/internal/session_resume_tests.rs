use super::session_resume::{
    apply_materialized_options, copy_auth_files, is_safe_subpath, materialize_resume_session,
    redact_refresh_token, strip_settings_for_resume,
};
use crate::session_store::{
    project_key_for_directory, InMemorySessionStore, SessionKey, SessionListSubkeysKey,
    SessionStore, SessionStoreEntry,
};
use crate::types::ClaudeAgentOptions;
use async_trait::async_trait;

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Delegates to an in-memory store but fails `list_subkeys`.
struct SubkeyFailingStore {
    inner: InMemorySessionStore,
    supports: bool,
}

#[async_trait]
impl SessionStore for SubkeyFailingStore {
    fn supports_list_subkeys(&self) -> bool {
        self.supports
    }

    async fn append(&self, key: SessionKey, entries: Vec<SessionStoreEntry>) -> crate::Result<()> {
        self.inner.append(key, entries).await
    }

    async fn load(&self, key: SessionKey) -> crate::Result<Option<Vec<SessionStoreEntry>>> {
        self.inner.load(key).await
    }

    async fn list_subkeys(&self, _key: SessionListSubkeysKey) -> crate::Result<Vec<String>> {
        Err(crate::ClaudeSDKError::Session("subkeys down".to_string()))
    }
}

#[tokio::test]
async fn resume_temp_dir_is_private_and_subkeys_keep_extension_suffix() {
    let cwd = std::env::temp_dir().join(format!("claude-rust-private-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let project_key = project_key_for_directory(Some(&cwd));
    let session_id = uuid::Uuid::new_v4().to_string();
    let store = InMemorySessionStore::new();
    store
        .append(
            SessionKey {
                project_key: project_key.clone(),
                session_id: session_id.clone(),
                subpath: None,
            },
            vec![entry("caf\u{e9}")],
        )
        .await
        .unwrap();
    store
        .append(
            SessionKey {
                project_key: project_key.clone(),
                session_id: session_id.clone(),
                subpath: Some("subagents/agent-a.b".to_string()),
            },
            vec![
                entry("sub"),
                serde_json::json!({"type": "agent_metadata", "agentType": "old"})
                    .as_object()
                    .unwrap()
                    .clone(),
                serde_json::json!({"agentType": "new", "type": "agent_metadata", "toolUseId": "t"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ],
        )
        .await
        .unwrap();
    let mut env = std::collections::HashMap::new();
    env.insert(
        "CLAUDE_CONFIG_DIR".to_string(),
        cwd.join("no-such-config").to_string_lossy().to_string(),
    );
    let options = ClaudeAgentOptions::builder()
        .cwd(cwd.to_string_lossy().to_string())
        .env(env)
        .session_store(store)
        .resume(session_id.clone())
        .build();

    let materialized = materialize_resume_session(&options)
        .await
        .unwrap()
        .expect("materialized");
    let name = materialized
        .config_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(name.starts_with("claude-resume-"), "{name}");
    let project_dir = materialized.config_dir.join("projects").join(&project_key);
    let main = project_dir.join(format!("{session_id}.jsonl"));
    // Compact Python JSON with ensure_ascii.
    let content = std::fs::read_to_string(&main).unwrap();
    assert!(content.contains(&format!("caf{}u00e9", '\\')), "{content}");
    let sub = project_dir
        .join(&session_id)
        .join("subagents/agent-a.b.jsonl");
    let meta = project_dir
        .join(&session_id)
        .join("subagents/agent-a.b.meta.json");
    assert!(sub.exists());
    assert_eq!(
        std::fs::read_to_string(&meta).unwrap(),
        r#"{"agentType": "new", "toolUseId": "t"}"#
    );
    #[cfg(unix)]
    {
        assert_eq!(mode(&materialized.config_dir), 0o700);
        assert_eq!(mode(&main), 0o600);
        assert_eq!(mode(&sub), 0o600);
        assert_eq!(mode(&meta), 0o600);
    }

    materialized.cleanup().await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn list_subkeys_errors_abort_resume_and_unsupported_stores_skip_it() {
    let cwd = std::env::temp_dir().join(format!("claude-rust-subkey-err-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let project_key = project_key_for_directory(Some(&cwd));
    let session_id = uuid::Uuid::new_v4().to_string();
    for supports in [true, false] {
        let inner = InMemorySessionStore::new();
        inner
            .append(
                SessionKey {
                    project_key: project_key.clone(),
                    session_id: session_id.clone(),
                    subpath: None,
                },
                vec![entry("main")],
            )
            .await
            .unwrap();
        let mut env = std::collections::HashMap::new();
        env.insert(
            "CLAUDE_CONFIG_DIR".to_string(),
            cwd.join("no-such-config").to_string_lossy().to_string(),
        );
        let options = ClaudeAgentOptions::builder()
            .cwd(cwd.to_string_lossy().to_string())
            .env(env)
            .session_store(SubkeyFailingStore { inner, supports })
            .resume(session_id.clone())
            .build();
        let result = materialize_resume_session(&options).await;
        if supports {
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains(&format!(
                    "SessionStore.list_subkeys() for session {session_id} failed"
                )),
                "{err}"
            );
        } else {
            result.unwrap().expect("materialized").cleanup().await;
        }
    }
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn copy_auth_files_seeds_credentials_claude_json_and_stripped_settings() {
    let root = std::env::temp_dir().join(format!("claude-rust-auth-{}", uuid::Uuid::new_v4()));
    let source = root.join("source");
    let dest = root.join("dest");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(
        source.join(".credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#,
    )
    .unwrap();
    std::fs::write(source.join(".claude.json"), "{\"x\": 1}").unwrap();
    std::fs::write(
        source.join("settings.json"),
        r#"{"apiKeyHelper":"helper","enabledPlugins":{"p":true},"env":{"CLAUDE_CONFIG_DIR":"/x","A":"b"}}"#,
    )
    .unwrap();
    std::fs::write(source.join("cowork_settings.json"), "{ \"keep\": true }").unwrap();
    let mut env = std::collections::HashMap::new();
    env.insert(
        "CLAUDE_CONFIG_DIR".to_string(),
        source.to_string_lossy().to_string(),
    );

    copy_auth_files(&dest, &env).await;

    assert_eq!(
        std::fs::read_to_string(dest.join(".credentials.json")).unwrap(),
        r#"{"claudeAiOauth": {"accessToken": "a"}}"#
    );
    assert_eq!(
        std::fs::read_to_string(dest.join(".claude.json")).unwrap(),
        "{\"x\": 1}"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("settings.json")).unwrap(),
        r#"{"apiKeyHelper":"helper","env":{"A":"b"}}"#
    );
    // Nothing stripped: original bytes are kept.
    assert_eq!(
        std::fs::read_to_string(dest.join("cowork_settings.json")).unwrap(),
        "{ \"keep\": true }"
    );
    #[cfg(unix)]
    for name in [
        ".credentials.json",
        ".claude.json",
        "settings.json",
        "cowork_settings.json",
    ] {
        assert_eq!(mode(&dest.join(name)), 0o600, "{name}");
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn settings_strip_and_credential_redaction_match_python() {
    // BOM-prefixed settings parse; non-object / invalid content is untouched.
    let bom = b"\xef\xbb\xbf{\"extraKnownMarketplaces\":{},\"a\":1}";
    assert_eq!(strip_settings_for_resume(bom), b"{\"a\":1}".to_vec());
    assert_eq!(strip_settings_for_resume(b"[1]"), b"[1]".to_vec());
    assert_eq!(strip_settings_for_resume(b"{bad"), b"{bad".to_vec());
    assert_eq!(
        strip_settings_for_resume(b"{\"env\": \"not-an-object\"}"),
        b"{\"env\": \"not-an-object\"}".to_vec()
    );
    // Only rewritten when a refreshToken is present.
    assert_eq!(
        redact_refresh_token(r#"{"claudeAiOauth":{"accessToken":"a"}}"#),
        None
    );
    assert_eq!(redact_refresh_token("not json"), None);
}

fn entry(content: &str) -> SessionStoreEntry {
    serde_json::json!({
        "type": "user",
        "uuid": uuid::Uuid::new_v4().to_string(),
        "message": {"content": content},
    })
    .as_object()
    .unwrap()
    .clone()
}

#[tokio::test]
async fn materializes_explicit_resume_to_temp_config_dir() {
    let cwd = std::env::temp_dir().join(format!("claude-rust-resume-cwd-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let project_key = project_key_for_directory(Some(&cwd));
    let session_id = uuid::Uuid::new_v4().to_string();
    let store = InMemorySessionStore::new();
    store
        .append(
            SessionKey {
                project_key,
                session_id: session_id.clone(),
                subpath: None,
            },
            vec![entry("hello")],
        )
        .await
        .unwrap();
    let options = ClaudeAgentOptions::builder()
        .cwd(cwd.to_string_lossy().to_string())
        .session_store(store)
        .resume(session_id.clone())
        .build();

    let materialized = materialize_resume_session(&options)
        .await
        .unwrap()
        .expect("materialized");

    let jsonl = tokio::fs::read_to_string(
        materialized
            .config_dir
            .join("projects")
            .join(project_key_for_directory(Some(&cwd)))
            .join(format!("{session_id}.jsonl")),
    )
    .await
    .unwrap();
    assert!(jsonl.contains("hello"));

    let applied = apply_materialized_options(&options, &materialized);
    assert_eq!(applied.resume.as_deref(), Some(session_id.as_str()));
    assert!(!applied.continue_conversation);
    assert_eq!(
        applied.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
        Some(materialized.config_dir.to_string_lossy().as_ref())
    );

    materialized.cleanup().await;
    assert!(!materialized.config_dir.exists());
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn materializes_continue_from_newest_non_sidechain_session() {
    let cwd =
        std::env::temp_dir().join(format!("claude-rust-continue-cwd-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let project_key = project_key_for_directory(Some(&cwd));
    let sidechain_id = uuid::Uuid::new_v4().to_string();
    let main_id = uuid::Uuid::new_v4().to_string();
    let store = InMemorySessionStore::new();
    let mut sidechain = entry("sidechain");
    sidechain.insert("isSidechain".to_string(), serde_json::Value::Bool(true));
    store
        .append(
            SessionKey {
                project_key: project_key.clone(),
                session_id: main_id.clone(),
                subpath: None,
            },
            vec![entry("main")],
        )
        .await
        .unwrap();
    store
        .append(
            SessionKey {
                project_key,
                session_id: sidechain_id,
                subpath: None,
            },
            vec![sidechain],
        )
        .await
        .unwrap();
    let options = ClaudeAgentOptions::builder()
        .cwd(cwd.to_string_lossy().to_string())
        .session_store(store)
        .continue_conversation(true)
        .build();

    let materialized = materialize_resume_session(&options)
        .await
        .unwrap()
        .expect("materialized");

    assert_eq!(materialized.resume_session_id, main_id);
    materialized.cleanup().await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn materializes_safe_subkeys_and_metadata_sidecars() {
    let cwd = std::env::temp_dir().join(format!("claude-rust-subkey-cwd-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let project_key = project_key_for_directory(Some(&cwd));
    let session_id = uuid::Uuid::new_v4().to_string();
    let store = InMemorySessionStore::new();
    store
        .append(
            SessionKey {
                project_key: project_key.clone(),
                session_id: session_id.clone(),
                subpath: None,
            },
            vec![entry("main")],
        )
        .await
        .unwrap();
    store
        .append(
            SessionKey {
                project_key: project_key.clone(),
                session_id: session_id.clone(),
                subpath: Some("subagents/agent-1".to_string()),
            },
            vec![
                entry("subagent"),
                serde_json::json!({"type": "agent_metadata", "name": "reviewer"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ],
        )
        .await
        .unwrap();
    let options = ClaudeAgentOptions::builder()
        .cwd(cwd.to_string_lossy().to_string())
        .session_store(store)
        .resume(session_id.clone())
        .build();

    let materialized = materialize_resume_session(&options)
        .await
        .unwrap()
        .expect("materialized");
    let session_dir = materialized
        .config_dir
        .join("projects")
        .join(project_key)
        .join(session_id);

    let transcript = tokio::fs::read_to_string(session_dir.join("subagents/agent-1.jsonl"))
        .await
        .unwrap();
    assert!(transcript.contains("subagent"));
    let metadata = tokio::fs::read_to_string(session_dir.join("subagents/agent-1.meta.json"))
        .await
        .unwrap();
    assert!(metadata.contains("reviewer"));
    assert!(!metadata.contains("agent_metadata"));

    materialized.cleanup().await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn rejects_invalid_resume_id_without_materializing() {
    let options = ClaudeAgentOptions::builder()
        .session_store(InMemorySessionStore::new())
        .resume("../../etc/passwd")
        .build();

    assert!(materialize_resume_session(&options)
        .await
        .unwrap()
        .is_none());
}

#[test]
fn validates_subpaths() {
    let session_dir =
        std::env::temp_dir().join(format!("claude-rust-subpath-{}", uuid::Uuid::new_v4()));
    assert!(is_safe_subpath("subagents/agent-1", &session_dir));
    assert!(is_safe_subpath(
        "subagents/workflows/run-1/agent-a.b",
        &session_dir
    ));
    assert!(!is_safe_subpath("", &session_dir));
    assert!(!is_safe_subpath("../escape", &session_dir));
    assert!(!is_safe_subpath("subagents/./agent-1", &session_dir));
    assert!(!is_safe_subpath("subagents\\..\\escape", &session_dir));
    assert!(!is_safe_subpath("/absolute", &session_dir));
    assert!(!is_safe_subpath("\\absolute", &session_dir));
    assert!(!is_safe_subpath("C:\\escape", &session_dir));
    assert!(!is_safe_subpath("C:escape", &session_dir));
    assert!(!is_safe_subpath("sub\0agents", &session_dir));
}
