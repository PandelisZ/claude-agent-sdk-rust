//! Batching layer between `transcript_mirror` stdout frames and a
//! `SessionStore` (upstream `_internal/transcript_mirror_batcher.py`).
//!
//! [`TranscriptMirrorBatcher::enqueue_value`] never waits on the store: it
//! buffers the frame and, once a threshold is crossed (or on every frame in
//! eager mode), hands the buffer to a background drain task, so a slow
//! `SessionStore::append` never stalls the caller (the router task, which
//! must keep routing control responses and requests). The drain task works
//! through batches one at a time, in order, so appends keep their order.
//! [`TranscriptMirrorBatcher::flush`] hands over what is left and waits for
//! every batch handed over so far. Store failures (and panics) become
//! `mirror_error` messages delivered through the error sink.

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use tokio::sync::{mpsc, oneshot};

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

/// Receives each `mirror_error` message (Python `on_error`). Called from the
/// drain task; it must not block.
pub type MirrorErrorSink = Arc<dyn Fn(Message) + Send + Sync>;

#[derive(Debug, Clone)]
struct PendingMirrorFrame {
    file_path: PathBuf,
    entries: Vec<SessionStoreEntry>,
}

/// One batch for the drain task; `done` is answered once the batch, and
/// every batch before it, has been sent and its errors reported.
struct DrainJob {
    frames: Vec<PendingMirrorFrame>,
    done: Option<oneshot::Sender<()>>,
}

/// What the drain task needs to send a batch.
struct Drainer {
    store: SessionStoreHandle,
    projects_dir: PathBuf,
    send_timeout: Duration,
    backoff: [Duration; 2],
    on_error: Option<MirrorErrorSink>,
}

pub struct TranscriptMirrorBatcher {
    store: SessionStoreHandle,
    projects_dir: PathBuf,
    send_timeout: Duration,
    backoff: [Duration; 2],
    pending: Vec<PendingMirrorFrame>,
    pending_entries: usize,
    pending_bytes: usize,
    flush_mode: SessionStoreFlushMode,
    on_error: Option<MirrorErrorSink>,
    /// The drain task's queue, started on first use. Dropping the batcher
    /// closes it; the task finishes the batches already queued, then exits.
    drain: Option<mpsc::UnboundedSender<DrainJob>>,
}

impl std::fmt::Debug for TranscriptMirrorBatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptMirrorBatcher")
            .field("projects_dir", &self.projects_dir)
            .field("pending_entries", &self.pending_entries)
            .field("pending_bytes", &self.pending_bytes)
            .field("flush_mode", &self.flush_mode)
            .finish_non_exhaustive()
    }
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
            on_error: None,
            drain: None,
        })
    }

    /// Where `mirror_error` messages go (set before the first enqueue).
    /// Without a sink failures are only logged.
    pub fn set_error_sink(&mut self, sink: MirrorErrorSink) {
        self.on_error = Some(sink);
    }

    /// Buffer one `transcript_mirror` frame. Never waits on the store: an
    /// eager or over-threshold flush runs in the background drain task.
    /// Must be called within a Tokio runtime.
    pub fn enqueue_value(&mut self, value: &serde_json::Value) -> Result<()> {
        let Some(file_path) = value.get("filePath").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        let entries = value
            .get("entries")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.as_object().cloned())
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
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
            self.hand_off(None);
        }
        Ok(())
    }

    /// Send everything pending, after any batch already handed to the drain
    /// task, and wait until all of it has reached the store (or failed and
    /// been reported through the error sink). Never fails.
    pub async fn flush(&mut self) {
        if self.pending.is_empty() && self.drain.is_none() {
            return;
        }
        let (done, finished) = oneshot::channel();
        self.hand_off(Some(done));
        // An error means the drain task is gone; nothing is left to wait for.
        let _ = finished.await;
    }

    /// Detach the pending buffer and queue it for the drain task.
    fn hand_off(&mut self, done: Option<oneshot::Sender<()>>) {
        let frames = std::mem::take(&mut self.pending);
        self.pending_entries = 0;
        self.pending_bytes = 0;
        if frames.is_empty() && done.is_none() {
            return;
        }
        let job = DrainJob { frames, done };
        let job = match &self.drain {
            None => job,
            Some(drain) => match drain.send(job) {
                Ok(()) => return,
                // The drain task is gone (it only exits once its queue
                // closes, so the runtime dropped it): start a fresh one.
                Err(mpsc::error::SendError(job)) => job,
            },
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let drainer = Drainer {
            store: self.store.clone(),
            projects_dir: self.projects_dir.clone(),
            send_timeout: self.send_timeout,
            backoff: self.backoff,
            on_error: self.on_error.clone(),
        };
        tokio::spawn(drainer.run(rx));
        let _ = tx.send(job);
        self.drain = Some(tx);
    }

    pub fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    pub fn pending_entries(&self) -> usize {
        self.pending_entries
    }
}

impl Drainer {
    /// Work through the batches in order until the batcher is dropped.
    async fn run(self, mut jobs: mpsc::UnboundedReceiver<DrainJob>) {
        while let Some(DrainJob { frames, done }) = jobs.recv().await {
            if !frames.is_empty() {
                // `append_with_retry` contains store failures and panics;
                // this guards the rest so later batches still go out.
                let errors = AssertUnwindSafe(self.send_batch(frames))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|panic| {
                        tracing::error!(
                            "[TranscriptMirrorBatcher] flush panicked: {}",
                            panic_message(panic.as_ref())
                        );
                        Vec::new()
                    });
                // Report before answering `done`, so a flush's caller has
                // seen the errors by the time it resumes.
                for error in errors {
                    self.report(error);
                }
            }
            if let Some(done) = done {
                let _ = done.send(());
            }
        }
    }

    fn report(&self, message: Message) {
        if let Some(sink) = &self.on_error {
            if std::panic::catch_unwind(AssertUnwindSafe(|| sink(message))).is_err() {
                tracing::error!("[TranscriptMirrorBatcher] on_error callback panicked");
            }
        }
    }

    /// Coalesce by file path in first-seen order (entries keep enqueue
    /// order) and append each; returns the `mirror_error` messages.
    async fn send_batch(&self, frames: Vec<PendingMirrorFrame>) -> Vec<Message> {
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

        let mut errors = Vec::new();
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
                errors.push(mirror_error_message(Some(key), error));
            }
        }
        errors
    }

    /// Up to [`MIRROR_APPEND_MAX_ATTEMPTS`] attempts with backoff, each bounded
    /// by the send timeout. A timeout is not retried (the in-flight call may
    /// still land); a panicking store counts as a failed attempt. Returns the
    /// last error's message.
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
            let append =
                AssertUnwindSafe(self.store.append(key.clone(), entries.clone())).catch_unwind();
            let error = match tokio::time::timeout(self.send_timeout, append).await {
                Ok(Ok(Ok(()))) => return Ok(()),
                Ok(Ok(Err(error))) => error.to_string(),
                Ok(Err(panic)) => format!(
                    "SessionStore.append panicked: {}",
                    panic_message(panic.as_ref())
                ),
                Err(_) => {
                    tracing::debug!(
                        "[TranscriptMirrorBatcher] append timed out after {:?} -- not retrying",
                        self.send_timeout
                    );
                    // Python reports `str(TimeoutError())`, an empty message.
                    return Err(String::new());
                }
            };
            tracing::debug!(
                "[TranscriptMirrorBatcher] append attempt {}/{} failed: {error}",
                attempt + 1,
                MIRROR_APPEND_MAX_ATTEMPTS
            );
            last_error = error;
        }
        Err(last_error)
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic".to_string()
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
    data.insert(
        "subtype".to_string(),
        serde_json::json!(crate::types::MirrorErrorMessage::SUBTYPE),
    );
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

    type Collected = Arc<std::sync::Mutex<Vec<Message>>>;

    fn collecting(mut batcher: TranscriptMirrorBatcher) -> (TranscriptMirrorBatcher, Collected) {
        let errors: Collected = Arc::default();
        let sink = errors.clone();
        batcher.set_error_sink(Arc::new(move |message| sink.lock().unwrap().push(message)));
        (batcher, errors)
    }

    fn take(errors: &Collected) -> Vec<Message> {
        std::mem::take(&mut *errors.lock().unwrap())
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
        let (mut batcher, errors) =
            collecting(TranscriptMirrorBatcher::from_options(&options).expect("batcher"));

        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "1"))
            .unwrap();
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "2"))
            .unwrap();
        assert_eq!(batcher.pending_entries(), 2);

        batcher.flush().await;
        let errors = take(&errors);
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
        let (mut batcher, _errors) =
            collecting(TranscriptMirrorBatcher::from_options(&options).expect("batcher"));
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "zzz", "1"))
            .unwrap();
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "aaa", "2"))
            .unwrap();
        batcher.flush().await;
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
        let (mut batcher, errors) = collecting(fast(
            TranscriptMirrorBatcher::from_options(&options).expect("batcher"),
        ));
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "1"))
            .unwrap();
        batcher.flush().await;
        let errors = take(&errors);
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
        let (mut batcher, errors) = collecting(fast(
            TranscriptMirrorBatcher::from_options(&options).expect("batcher"),
        ));

        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "1"))
            .unwrap();
        batcher.flush().await;
        let errors = take(&errors);

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
        let (mut batcher, errors) = collecting(fast(
            TranscriptMirrorBatcher::from_options(&options).expect("batcher"),
        ));
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "1"))
            .unwrap();
        batcher.flush().await;
        let errors = take(&errors);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(errors.len(), 1);
    }

    /// Sleeps on every append, then records the entry uuids in order.
    struct SlowStore {
        delay: Duration,
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl SessionStore for SlowStore {
        async fn append(&self, _key: SessionKey, entries: Vec<SessionStoreEntry>) -> Result<()> {
            tokio::time::sleep(self.delay).await;
            let mut seen = self.seen.lock().unwrap();
            for entry in entries {
                seen.push(entry["uuid"].as_str().unwrap_or_default().to_string());
            }
            Ok(())
        }

        async fn load(&self, _key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn eager_enqueue_hands_off_without_waiting_and_keeps_append_order() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (options, projects_dir) = options_with_store(SlowStore {
            delay: Duration::from_millis(200),
            seen: seen.clone(),
        });
        let options = ClaudeAgentOptions {
            session_store_flush: SessionStoreFlushMode::Eager,
            ..options
        };
        let (mut batcher, errors) =
            collecting(TranscriptMirrorBatcher::from_options(&options).expect("batcher"));

        let started = std::time::Instant::now();
        for uuid in ["1", "2", "3"] {
            batcher
                .enqueue_value(&mirror_frame(&projects_dir, "session-1", uuid))
                .unwrap();
            assert_eq!(batcher.pending_entries(), 0, "eager mode hands off at once");
        }
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "enqueue waited on the store: {:?}",
            started.elapsed()
        );
        assert!(seen.lock().unwrap().is_empty());

        // flush waits for every batch already handed off, in order.
        batcher.flush().await;
        assert_eq!(*seen.lock().unwrap(), ["1", "2", "3"]);
        assert!(take(&errors).is_empty());
    }

    #[tokio::test]
    async fn over_threshold_enqueue_drains_in_the_background() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (options, projects_dir) = options_with_store(SlowStore {
            delay: Duration::from_millis(200),
            seen: seen.clone(),
        });
        let (mut batcher, _errors) =
            collecting(TranscriptMirrorBatcher::from_options(&options).expect("batcher"));
        let started = std::time::Instant::now();
        for index in 0..=MAX_PENDING_ENTRIES {
            batcher
                .enqueue_value(&mirror_frame(
                    &projects_dir,
                    "session-1",
                    &index.to_string(),
                ))
                .unwrap();
        }
        assert_eq!(batcher.pending_entries(), 0);
        assert!(started.elapsed() < Duration::from_millis(150));
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "last"))
            .unwrap();
        batcher.flush().await;
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), MAX_PENDING_ENTRIES + 2);
        assert_eq!(seen.last().map(String::as_str), Some("last"));
    }

    struct PanickingStore {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SessionStore for PanickingStore {
        async fn append(&self, _key: SessionKey, _entries: Vec<SessionStoreEntry>) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("store exploded");
        }

        async fn load(&self, _key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn panicking_store_is_reported_as_a_mirror_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (options, projects_dir) = options_with_store(PanickingStore {
            calls: calls.clone(),
        });
        let (mut batcher, errors) = collecting(fast(
            TranscriptMirrorBatcher::from_options(&options).expect("batcher"),
        ));
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "1"))
            .unwrap();
        batcher.flush().await;
        // A panic is a failed attempt: retried, then reported.
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let reported = take(&errors);
        assert_eq!(reported.len(), 1);
        match &reported[0] {
            Message::MirrorErrorMsg(message) => {
                assert!(
                    message.error.contains("store exploded"),
                    "{}",
                    message.error
                );
            }
            other => panic!("expected mirror error, got {other:?}"),
        }

        // The drain task survives for later batches.
        batcher
            .enqueue_value(&mirror_frame(&projects_dir, "session-1", "2"))
            .unwrap();
        batcher.flush().await;
        assert_eq!(calls.load(Ordering::SeqCst), 6);
        assert_eq!(take(&errors).len(), 1);
    }

    #[test]
    fn mirror_error_without_key_has_null_key_and_empty_session_id() {
        let Message::MirrorErrorMsg(message) = mirror_error_message(None, "boom") else {
            panic!("expected mirror error");
        };
        assert_eq!(message.data["key"], serde_json::Value::Null);
        assert_eq!(message.data["session_id"], "");
        assert_eq!(message.error, "boom");
        assert_eq!(message.subtype(), "mirror_error");
    }

    #[test]
    fn mirror_error_data_matches_python_report_mirror_error_shape() {
        let key = SessionKey {
            project_key: "proj".to_string(),
            session_id: "sess".to_string(),
            subpath: None,
        };
        let message = mirror_error_message(Some(key), "boom");
        let Message::MirrorErrorMsg(mirror) = &message else {
            panic!("expected mirror error");
        };
        // Same keys, in the same order, as Python's report_mirror_error dict.
        let keys: Vec<&str> = mirror.data.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["type", "subtype", "error", "key", "uuid", "session_id"]
        );
        assert_eq!(mirror.data["type"], "system");
        assert_eq!(mirror.data["subtype"], mirror.subtype());
        assert_eq!(
            mirror.data["key"],
            serde_json::json!({"project_key": "proj", "session_id": "sess"})
        );
        assert_eq!(mirror.data["session_id"], "sess");
        assert_eq!(
            mirror.session_key().map(|k| k.session_id),
            Some("sess".to_string())
        );
        assert_eq!(message.as_system().map(|(s, _)| s), Some("mirror_error"));
    }
}
