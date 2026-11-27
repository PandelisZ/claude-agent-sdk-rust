//! Batching layer between `transcript_mirror` stdout frames and a
//! `SessionStore` (upstream `_internal/transcript_mirror_batcher.py`).

use std::path::PathBuf;
use std::time::Duration;

use crate::error::Result;
use crate::session_store::{
    file_path_to_session_key, SessionKey, SessionStoreEntry, SessionStoreHandle,
};
use crate::types::{ClaudeAgentOptions, Message, SessionStoreFlushMode};

const MAX_PENDING_ENTRIES: usize = 500;
const MAX_PENDING_BYTES: usize = 1 << 20;
/// Per-attempt timeout for `SessionStore::append` (`SEND_TIMEOUT_SECONDS`).
const SEND_TIMEOUT: Duration = Duration::from_secs(60);
/// Attempts per batch (`MIRROR_APPEND_MAX_ATTEMPTS`).
const MIRROR_APPEND_MAX_ATTEMPTS: usize = 3;
/// Delay before each retry (`MIRROR_APPEND_BACKOFF_S`).
const MIRROR_APPEND_BACKOFF: [Duration; 2] =
    [Duration::from_millis(200), Duration::from_millis(800)];

#[derive(Debug, Clone)]
struct PendingMirrorFrame {
    file_path: PathBuf,
    entries: Vec<SessionStoreEntry>,
}

#[derive(Debug, Clone)]
pub struct TranscriptMirrorBatcher {
    store: SessionStoreHandle,
    projects_dir: PathBuf,
    send_timeout: Duration,
    backoff: [Duration; 2],
    pending: Vec<PendingMirrorFrame>,
    pending_entries: usize,
    pending_bytes: usize,
    flush_mode: SessionStoreFlushMode,
    mirror_errors: Vec<Message>,
}

impl TranscriptMirrorBatcher {
    pub fn from_options(options: &ClaudeAgentOptions) -> Option<Self> {
        let store = options.session_store.clone()?;
        Some(Self {
            store,
            projects_dir: projects_dir_for_options(options),
            send_timeout: SEND_TIMEOUT,
            backoff: MIRROR_APPEND_BACKOFF,
            pending: Vec::new(),
            pending_entries: 0,
            pending_bytes: 0,
            flush_mode: options.session_store_flush,
            mirror_errors: Vec::new(),
        })
    }

    pub async fn enqueue_value(&mut self, value: &serde_json::Value) -> Result<Vec<Message>> {
        let Some(file_path) = value.get("filePath").and_then(|v| v.as_str()) else {
            return Ok(Vec::new());
        };
        let entries = value
            .get("entries")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.as_object().cloned())
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let bytes = serde_json::to_vec(&entries)?.len();
        self.pending_entries += entries.len();
        self.pending_bytes += bytes;
        self.pending.push(PendingMirrorFrame {
            file_path: PathBuf::from(file_path),
            entries,
        });

        if self.flush_mode == SessionStoreFlushMode::Eager
            || self.pending_entries > MAX_PENDING_ENTRIES
            || self.pending_bytes > MAX_PENDING_BYTES
        {
            return self.flush().await;
        }

        Ok(Vec::new())
    }

    pub async fn flush(&mut self) -> Result<Vec<Message>> {
        if self.pending.is_empty() {
            return Ok(std::mem::take(&mut self.mirror_errors));
        }

        let frames = std::mem::take(&mut self.pending);
        self.pending_entries = 0;
        self.pending_bytes = 0;

        // Coalesce by file path in first-seen order; entries keep enqueue order.
        let mut by_path: Vec<(PathBuf, Vec<SessionStoreEntry>)> = Vec::new();
        for frame in frames {
            match by_path
                .iter_mut()
                .find(|(path, _)| *path == frame.file_path)
            {
                Some((_, bucket)) => bucket.extend(frame.entries),
                None => by_path.push((frame.file_path, frame.entries)),
            }
        }

        for (file_path, entries) in by_path {
            if entries.is_empty() {
                continue;
            }
            let Some(key) = file_path_to_session_key(&file_path, &self.projects_dir) else {
                tracing::warn!(
                    "[SessionStore] dropping mirror frame: filePath {} is not under {} -- \
                     subprocess CLAUDE_CONFIG_DIR likely differs from parent",
                    file_path.display(),
                    self.projects_dir.display()
                );
                continue;
            };
            if let Err(error) = self.append_with_retry(&key, entries).await {
                tracing::error!(
                    "[TranscriptMirrorBatcher] flush failed for {}: {error}",
                    file_path.display()
                );
                self.mirror_errors
                    .push(mirror_error_message(Some(key), error));
            }
        }

        Ok(std::mem::take(&mut self.mirror_errors))
    }

    /// Up to [`MIRROR_APPEND_MAX_ATTEMPTS`] attempts with backoff, each bounded
    /// by the send timeout. A timeout is not retried (the in-flight call may
    /// still land). Returns the last error's message.
    async fn append_with_retry(
        &self,
        key: &SessionKey,
        entries: Vec<SessionStoreEntry>,
    ) -> std::result::Result<(), String> {
        let mut last_error = String::new();
        for attempt in 0..MIRROR_APPEND_MAX_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(self.backoff[attempt - 1]).await;
            }
            match tokio::time::timeout(
                self.send_timeout,
                self.store.append(key.clone(), entries.clone()),
            )
            .await
            {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(error)) => {
                    tracing::debug!(
                        "[TranscriptMirrorBatcher] append attempt {}/{} failed: {error}",
                        attempt + 1,
                        MIRROR_APPEND_MAX_ATTEMPTS
                    );
                    last_error = error.to_string();
                }
                Err(_) => {
                    tracing::debug!(
                        "[TranscriptMirrorBatcher] append timed out after {:?} -- not retrying",
                        self.send_timeout
                    );
                    // Python reports `str(TimeoutError())`, an empty message.
                    return Err(String::new());
                }
            }
        }
        Err(last_error)
    }

    pub fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    pub fn pending_entries(&self) -> usize {
        self.pending_entries
    }
}

/// The projects dir the subprocess writes to: `options.env` CLAUDE_CONFIG_DIR,
/// else the process env (empty = unset), else `~/.claude` (NFC).
fn projects_dir_for_options(options: &ClaudeAgentOptions) -> PathBuf {
    crate::sessions::paths::projects_dir(Some(&options.env))
}

/// The SDK-synthesized `mirror_error` system message (Python
/// `report_mirror_error`); `data` is the full payload
/// `{type, subtype, error, key, uuid, session_id}`.
pub fn mirror_error_message(key: Option<SessionKey>, error: impl Into<String>) -> Message {
    let error = error.into();
    let key = key.map(|key| {
        let mut value = serde_json::Map::new();
        value.insert(
            "project_key".to_string(),
            serde_json::json!(key.project_key),
        );
        value.insert("session_id".to_string(), serde_json::json!(key.session_id));
        if let Some(subpath) = key.subpath {
            value.insert("subpath".to_string(), serde_json::json!(subpath));
        }
        value
    });
    let session_id = key
        .as_ref()
        .and_then(|key| key.get("session_id"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let mut data = serde_json::Map::new();
    data.insert("type".to_string(), serde_json::json!("system"));
    data.insert("subtype".to_string(), serde_json::json!("mirror_error"));
    data.insert("error".to_string(), serde_json::json!(error));
    data.insert(
        "key".to_string(),
        key.clone()
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null),
    );
    data.insert(
        "uuid".to_string(),
        serde_json::json!(uuid::Uuid::new_v4().to_string()),
    );
    data.insert("session_id".to_string(), serde_json::json!(session_id));
    Message::MirrorErrorMsg(crate::types::MirrorErrorMessage { key, error, data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_store::{InMemorySessionStore, SessionStore};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Fails the first `failures` appends, then records successes.
    struct FlakyStore {
        failures: usize,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SessionStore for FlakyStore {
        async fn append(&self, _key: SessionKey, _entries: Vec<SessionStoreEntry>) -> Result<()> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures {
                Err(crate::error::ClaudeSDKError::Session(
                    "store failed".to_string(),
                ))
            } else {
                Ok(())
            }
        }

        async fn load(&self, _key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
            Ok(None)
        }
    }

    struct HangingStore {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SessionStore for HangingStore {
        async fn append(&self, _key: SessionKey, _entries: Vec<SessionStoreEntry>) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            futures::future::pending::<()>().await;
            Ok(())
        }

        async fn load(&self, _key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
            Ok(None)
        }
    }

    fn mirror_frame(
        projects_dir: &std::path::Path,
        session: &str,
        uuid: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "type": "transcript_mirror",
            "filePath": projects_dir.join(format!("proj/{session}.jsonl")),
            "entries": [{
                "type": "user",
                "uuid": uuid,
                "message": {"content": format!("prompt {uuid}")}
            }]
        })
    }

    fn options_with_store<S: SessionStore + 'static>(
        store: S,
    ) -> (ClaudeAgentOptions, std::path::PathBuf) {
        let temp =
            std::env::temp_dir().join(format!("claude-rust-mirror-test-{}", uuid::Uuid::new_v4()));
        let mut env = std::collections::HashMap::new();
        env.insert(
            "CLAUDE_CONFIG_DIR".to_string(),
            temp.to_string_lossy().to_string(),
        );
        let options = ClaudeAgentOptions::builder()
            .env(env)
            .session_store(store)
            .build();
        (options, temp.join("projects"))
    }

    fn fast(mut batcher: TranscriptMirrorBatcher) -> TranscriptMirrorBatcher {
        batcher.backoff = [Duration::from_millis(1), Duration::from_millis(1)];
        batcher.send_timeout = Duration::from_millis(50);
        batcher
    }

    #[tokio::test]
    async fn batcher_coalesces_and_flushes_by_session_key() {
        let store = InMemorySessionStore::new();
        let (options, projects_dir) = options_with_store(store.clone());
        let mut batcher = TranscriptMirrorBatcher::from_options(&options).expect("batcher");

        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "1"))
            .await
            .unwrap();
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "2"))
            .await
            .unwrap();
        assert_eq!(batcher.pending_entries(), 2);

        let errors = batcher.flush().await.unwrap();
        assert!(errors.is_empty());

        let entries = store
            .load(SessionKey {
                project_key: "proj".to_string(),
                session_id: "session-1".to_string(),
                subpath: None,
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["uuid"], "1");
        assert_eq!(entries[1]["uuid"], "2");
    }

    #[tokio::test]
    async fn batcher_flushes_paths_in_first_seen_order() {
        let store = InMemorySessionStore::new();
        let (options, projects_dir) = options_with_store(store.clone());
        let mut batcher = TranscriptMirrorBatcher::from_options(&options).expect("batcher");
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "zzz", "1"))
            .await
            .unwrap();
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "aaa", "2"))
            .await
            .unwrap();
        batcher.flush().await.unwrap();
        let sessions = store.list_sessions("proj").await.unwrap();
        let zzz = sessions.iter().find(|s| s.session_id == "zzz").unwrap();
        let aaa = sessions.iter().find(|s| s.session_id == "aaa").unwrap();
        assert!(zzz.mtime < aaa.mtime, "zzz must be appended before aaa");
    }

    #[tokio::test]
    async fn batcher_retries_transient_failures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (options, projects_dir) = options_with_store(FlakyStore {
            failures: 2,
            calls: calls.clone(),
        });
        let mut batcher = fast(TranscriptMirrorBatcher::from_options(&options).expect("batcher"));
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "1"))
            .await
            .unwrap();
        let errors = batcher.flush().await.unwrap();
        assert!(errors.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn batcher_reports_store_failures_as_nonfatal_mirror_errors() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (options, projects_dir) = options_with_store(FlakyStore {
            failures: usize::MAX,
            calls: calls.clone(),
        });
        let mut batcher = fast(TranscriptMirrorBatcher::from_options(&options).expect("batcher"));

        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "1"))
            .await
            .unwrap();
        let errors = batcher.flush().await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            Message::MirrorErrorMsg(message) => {
                assert!(message.error.contains("store failed"));
                assert_eq!(
                    message.key.as_ref().and_then(|key| key.get("project_key")),
                    Some(&serde_json::json!("proj"))
                );
                assert_eq!(message.data["type"], "system");
                assert_eq!(message.data["subtype"], "mirror_error");
                assert_eq!(message.data["session_id"], "session-1");
                assert_eq!(message.data["key"]["session_id"], "session-1");
                assert!(message.data["uuid"].as_str().is_some());
                assert!(message.data.get("filePath").is_none());
            }
            other => panic!("expected mirror_error system message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn batcher_does_not_retry_timeouts() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (options, projects_dir) = options_with_store(HangingStore {
            calls: calls.clone(),
        });
        let mut batcher = fast(TranscriptMirrorBatcher::from_options(&options).expect("batcher"));
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "1"))
            .await
            .unwrap();
        let errors = batcher.flush().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn mirror_error_without_key_has_null_key_and_empty_session_id() {
        let Message::MirrorErrorMsg(message) = mirror_error_message(None, "boom") else {
            panic!("expected mirror error");
        };
        assert_eq!(message.data["key"], serde_json::Value::Null);
        assert_eq!(message.data["session_id"], "");
        assert_eq!(message.error, "boom");
    }
}
