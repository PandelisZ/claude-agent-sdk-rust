use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::error::Result;
use crate::session_summary::fold_session_summary;

use crate::sessions::paths::sanitize_path;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    pub project_key: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionListSubkeysKey {
    pub project_key: String,
    pub session_id: String,
}

pub type SessionStoreEntry = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStoreListEntry {
    pub session_id: String,
    pub mtime: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummaryEntry {
    pub session_id: String,
    pub mtime: i64,
    pub data: serde_json::Map<String, serde_json::Value>,
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn append(&self, key: SessionKey, entries: Vec<SessionStoreEntry>) -> Result<()>;
    async fn load(&self, key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>>;

    /// Whether [`SessionStore::list_sessions`] is implemented. Rust cannot
    /// detect an overridden default method, so stores that implement the
    /// optional method must also return `true` here.
    fn supports_list_sessions(&self) -> bool {
        false
    }

    /// Whether [`SessionStore::list_session_summaries`] is implemented. When
    /// `true`, `list_sessions_from_store` uses the summary fast path.
    fn supports_list_session_summaries(&self) -> bool {
        false
    }

    /// Whether [`SessionStore::list_subkeys`] is implemented. When `true`,
    /// resume materializes subagent transcripts and the subagent readers scan
    /// subkeys; `list_subagents_from_store` requires it.
    fn supports_list_subkeys(&self) -> bool {
        false
    }

    async fn list_sessions(&self, _project_key: &str) -> Result<Vec<SessionStoreListEntry>> {
        Err(crate::error::ClaudeSDKError::Session(
            "SessionStore::list_sessions is not implemented".to_string(),
        ))
    }

    async fn list_session_summaries(&self, _project_key: &str) -> Result<Vec<SessionSummaryEntry>> {
        Err(crate::error::ClaudeSDKError::Session(
            "SessionStore::list_session_summaries is not implemented".to_string(),
        ))
    }

    async fn delete(&self, _key: SessionKey) -> Result<()> {
        Ok(())
    }

    async fn list_subkeys(&self, _key: SessionListSubkeysKey) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

#[derive(Clone)]
pub struct SessionStoreHandle(Arc<dyn SessionStore>);

impl SessionStoreHandle {
    pub fn new<S>(store: S) -> Self
    where
        S: SessionStore + 'static,
    {
        Self(Arc::new(store))
    }

    pub async fn append(&self, key: SessionKey, entries: Vec<SessionStoreEntry>) -> Result<()> {
        self.0.append(key, entries).await
    }

    pub async fn load(&self, key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        self.0.load(key).await
    }

    pub fn supports_list_sessions(&self) -> bool {
        self.0.supports_list_sessions()
    }

    pub fn supports_list_session_summaries(&self) -> bool {
        self.0.supports_list_session_summaries()
    }

    pub fn supports_list_subkeys(&self) -> bool {
        self.0.supports_list_subkeys()
    }

    pub async fn list_sessions(&self, project_key: &str) -> Result<Vec<SessionStoreListEntry>> {
        self.0.list_sessions(project_key).await
    }

    pub async fn list_session_summaries(
        &self,
        project_key: &str,
    ) -> Result<Vec<SessionSummaryEntry>> {
        self.0.list_session_summaries(project_key).await
    }

    pub async fn delete(&self, key: SessionKey) -> Result<()> {
        self.0.delete(key).await
    }

    pub async fn list_subkeys(&self, key: SessionListSubkeysKey) -> Result<Vec<String>> {
        self.0.list_subkeys(key).await
    }
}

impl fmt::Debug for SessionStoreHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SessionStoreHandle")
            .field(&"<session_store>")
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct InMemorySessionStore {
    state: Arc<Mutex<InMemorySessionStoreState>>,
}

#[derive(Debug, Default)]
struct InMemorySessionStoreState {
    store: HashMap<String, Vec<SessionStoreEntry>>,
    mtimes: HashMap<String, i64>,
    summaries: HashMap<(String, String), SessionSummaryEntry>,
    last_mtime: i64,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_entries(&self, key: SessionKey) -> Vec<SessionStoreEntry> {
        let state = self.state.lock().await;
        state
            .store
            .get(&key_to_string(&key))
            .cloned()
            .unwrap_or_default()
    }

    pub async fn size(&self) -> usize {
        let state = self.state.lock().await;
        state
            .store
            .keys()
            .filter(|key| {
                key.find('/')
                    .is_some_and(|idx| !key[idx + 1..].contains('/'))
            })
            .count()
    }

    pub async fn clear(&self) {
        let mut state = self.state.lock().await;
        state.store.clear();
        state.mtimes.clear();
        state.summaries.clear();
        state.last_mtime = 0;
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    fn supports_list_sessions(&self) -> bool {
        true
    }

    fn supports_list_session_summaries(&self) -> bool {
        true
    }

    fn supports_list_subkeys(&self) -> bool {
        true
    }

    async fn append(&self, key: SessionKey, entries: Vec<SessionStoreEntry>) -> Result<()> {
        let mut state = self.state.lock().await;
        let store_key = key_to_string(&key);
        state
            .store
            .entry(store_key.clone())
            .or_default()
            .extend(entries.clone());
        let next = next_mtime(state.last_mtime);
        state.last_mtime = next;
        if key.subpath.is_none() {
            let summary_key = (key.project_key.clone(), key.session_id.clone());
            let mut summary =
                fold_session_summary(state.summaries.get(&summary_key), &key, &entries);
            summary.mtime = next;
            state.summaries.insert(summary_key, summary);
        }
        state.mtimes.insert(store_key, next);
        Ok(())
    }

    async fn load(&self, key: SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        let state = self.state.lock().await;
        Ok(state.store.get(&key_to_string(&key)).cloned())
    }

    async fn list_sessions(&self, project_key: &str) -> Result<Vec<SessionStoreListEntry>> {
        let state = self.state.lock().await;
        let prefix = format!("{project_key}/");
        let mut sessions = Vec::new();
        for key in state.store.keys() {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            if !rest.contains('/') {
                sessions.push(SessionStoreListEntry {
                    session_id: rest.to_string(),
                    mtime: *state.mtimes.get(key).unwrap_or(&0),
                });
            }
        }
        Ok(sessions)
    }

    async fn delete(&self, key: SessionKey) -> Result<()> {
        let mut state = self.state.lock().await;
        let store_key = key_to_string(&key);
        state.store.remove(&store_key);
        state.mtimes.remove(&store_key);
        if key.subpath.is_none() {
            state
                .summaries
                .remove(&(key.project_key.clone(), key.session_id.clone()));
            let prefix = format!("{}/{}/", key.project_key, key.session_id);
            let subkeys: Vec<_> = state
                .store
                .keys()
                .filter(|candidate| candidate.starts_with(&prefix))
                .cloned()
                .collect();
            for subkey in subkeys {
                state.store.remove(&subkey);
                state.mtimes.remove(&subkey);
            }
        }
        Ok(())
    }

    async fn list_session_summaries(&self, project_key: &str) -> Result<Vec<SessionSummaryEntry>> {
        let state = self.state.lock().await;
        Ok(state
            .summaries
            .iter()
            .filter(|((candidate_project, _), _)| candidate_project == project_key)
            .map(|(_, summary)| summary.clone())
            .collect())
    }

    async fn list_subkeys(&self, key: SessionListSubkeysKey) -> Result<Vec<String>> {
        let state = self.state.lock().await;
        let prefix = format!("{}/{}/", key.project_key, key.session_id);
        Ok(state
            .store
            .keys()
            .filter_map(|store_key| store_key.strip_prefix(&prefix).map(String::from))
            .collect())
    }
}

/// Derive the `SessionStore` `project_key` for a directory (default: the
/// current directory): realpath + NFC + the CLI's sanitization, so keys match
/// the on-disk project directory names.
pub fn project_key_for_directory(directory: Option<&Path>) -> String {
    let directory = directory.unwrap_or_else(|| Path::new("."));
    sanitize_path(&crate::sessions::paths::canonicalize_path(
        &directory.to_string_lossy(),
    ))
}

/// Derive a [`SessionKey`] from a transcript file path.
///
/// Main transcripts: `<projects_dir>/<project_key>/<session_id>.jsonl`;
/// subagent transcripts:
/// `<projects_dir>/<project_key>/<session_id>/subagents/agent-<id>.jsonl`.
///
/// Like Python's `os.path.relpath`, both paths are made absolute (against
/// the current directory) and normalized lexically (`.` and `..` resolved)
/// before comparing, so a relative or non-normalized `CLAUDE_CONFIG_DIR`
/// still matches the CLI's reported paths. Returns `None` when `file_path`
/// is not under `projects_dir` or has an unrecognized shape.
pub fn file_path_to_session_key(file_path: &Path, projects_dir: &Path) -> Option<SessionKey> {
    let file_path = lexical_absolute(file_path);
    let projects_dir = lexical_absolute(projects_dir);
    let relative = file_path.strip_prefix(&projects_dir).ok()?;
    let parts = normal_components(relative);
    if parts.len() < 2 {
        return None;
    }

    let project_key = parts[0].clone();
    let second = &parts[1];
    if parts.len() == 2 && second.ends_with(".jsonl") {
        return Some(SessionKey {
            project_key,
            session_id: second.trim_end_matches(".jsonl").to_string(),
            subpath: None,
        });
    }

    if parts.len() >= 4 {
        let mut subpath_parts = parts[2..].to_vec();
        if let Some(last) = subpath_parts.last_mut() {
            if last.ends_with(".jsonl") {
                *last = last.trim_end_matches(".jsonl").to_string();
            }
        }
        return Some(SessionKey {
            project_key,
            session_id: second.clone(),
            subpath: Some(subpath_parts.join("/")),
        });
    }

    None
}

fn key_to_string(key: &SessionKey) -> String {
    match &key.subpath {
        Some(subpath) if !subpath.is_empty() => {
            format!("{}/{}/{}", key.project_key, key.session_id, subpath)
        }
        _ => format!("{}/{}", key.project_key, key.session_id),
    }
}

fn next_mtime(last_mtime: i64) -> i64 {
    let now = chrono::Utc::now().timestamp_millis();
    if now <= last_mtime {
        last_mtime + 1
    } else {
        now
    }
}

/// `os.path.abspath`: join a relative path onto the current directory and
/// resolve `.` / `..` lexically (no filesystem access, symlinks untouched).
fn lexical_absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Like normpath, `..` at the root stays at the root.
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push(component);
                }
            }
            other => normalized.push(other),
        }
    }
    normalized
}

fn normal_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::paths::MAX_SANITIZED_LENGTH;

    fn entry(uuid: &str) -> SessionStoreEntry {
        let mut entry = serde_json::Map::new();
        entry.insert("type".to_string(), serde_json::json!("user"));
        entry.insert("uuid".to_string(), serde_json::json!(uuid));
        entry.insert(
            "message".to_string(),
            serde_json::json!({"content": format!("prompt {uuid}")}),
        );
        entry
    }

    #[test]
    fn derives_session_keys_from_main_and_subagent_paths() {
        let projects = Path::new("/tmp/claude/projects");

        assert_eq!(
            file_path_to_session_key(projects.join("proj/session-1.jsonl").as_path(), projects),
            Some(SessionKey {
                project_key: "proj".to_string(),
                session_id: "session-1".to_string(),
                subpath: None,
            })
        );
        assert_eq!(
            file_path_to_session_key(
                projects
                    .join("proj/session-1/subagents/agent-abc.jsonl")
                    .as_path(),
                projects
            ),
            Some(SessionKey {
                project_key: "proj".to_string(),
                session_id: "session-1".to_string(),
                subpath: Some("subagents/agent-abc".to_string()),
            })
        );
    }

    #[test]
    fn session_keys_tolerate_relative_and_non_normalized_projects_dir() {
        let expected = Some(SessionKey {
            project_key: "proj".to_string(),
            session_id: "session-1".to_string(),
            subpath: None,
        });
        // CLAUDE_CONFIG_DIR="/home/u/proj/../cfg": the CLI reports the
        // normalized path.
        assert_eq!(
            file_path_to_session_key(
                Path::new("/home/u/cfg/projects/proj/session-1.jsonl"),
                Path::new("/home/u/proj/../cfg/./projects"),
            ),
            expected
        );
        // Relative CLAUDE_CONFIG_DIR, absolute reported path.
        let cwd = std::env::current_dir().unwrap();
        let absolute = cwd.join(".claude-local/projects/proj/session-1.jsonl");
        assert_eq!(
            file_path_to_session_key(&absolute, Path::new(".claude-local/projects")),
            expected
        );
        // Relative on both sides still works.
        assert_eq!(
            file_path_to_session_key(
                Path::new(".claude-local/projects/proj/session-1.jsonl"),
                Path::new("./.claude-local/projects"),
            ),
            expected
        );
        // Escaping projects_dir via `..` is rejected.
        assert_eq!(
            file_path_to_session_key(
                Path::new("/home/u/cfg/projects/../other/proj/session-1.jsonl"),
                Path::new("/home/u/cfg/projects"),
            ),
            None
        );
    }

    #[test]
    fn sanitizes_project_keys_like_python_sdk() {
        assert_eq!(
            sanitize_path("/Users/alice/my project"),
            "-Users-alice-my-project"
        );
        let long = "a".repeat(MAX_SANITIZED_LENGTH + 1);
        assert!(sanitize_path(&long).starts_with(&"a".repeat(MAX_SANITIZED_LENGTH)));
        assert_eq!(crate::sessions::paths::simple_hash("abc"), "22ci");
    }

    #[tokio::test]
    async fn in_memory_store_lists_loads_and_cascades_delete() {
        let store = InMemorySessionStore::new();
        let main = SessionKey {
            project_key: "proj".to_string(),
            session_id: "session".to_string(),
            subpath: None,
        };
        let sub = SessionKey {
            subpath: Some("subagents/agent-1".to_string()),
            ..main.clone()
        };

        store.append(main.clone(), vec![entry("1")]).await.unwrap();
        store.append(sub.clone(), vec![entry("2")]).await.unwrap();

        assert_eq!(store.load(main.clone()).await.unwrap().unwrap().len(), 1);
        assert_eq!(store.list_sessions("proj").await.unwrap().len(), 1);
        let summaries = store.list_session_summaries("proj").await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].data["first_prompt"], "prompt 1");
        assert_eq!(
            store
                .list_subkeys(SessionListSubkeysKey {
                    project_key: "proj".to_string(),
                    session_id: "session".to_string(),
                })
                .await
                .unwrap(),
            vec!["subagents/agent-1".to_string()]
        );

        store.delete(main.clone()).await.unwrap();
        assert!(store.load(main).await.unwrap().is_none());
        assert!(store.load(sub).await.unwrap().is_none());
        assert!(store
            .list_session_summaries("proj")
            .await
            .unwrap()
            .is_empty());
    }
}
