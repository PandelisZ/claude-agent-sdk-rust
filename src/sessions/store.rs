//! `SessionStore`-backed session helpers (upstream `_internal/sessions.py`
//! and `_internal/session_mutations.py`, "SessionStore-backed" sections).

use crate::error::{ClaudeSDKError, Result};
use crate::internal::sessions_fs::apply_sort_limit_offset;
use crate::session_store::{
    SessionKey, SessionListSubkeysKey, SessionStoreEntry, SessionStoreHandle, SessionStoreListEntry,
};
use crate::session_summary::summary_entry_to_sdk_info;
use crate::sessions::fork_core::iso_now;
use crate::sessions::paths::{canonicalize_path, is_valid_uuid, sanitize_path};
use crate::sessions::{chain, lite, sanitize, SessionMessage};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Upper bound on concurrent `store.load()` calls in `list_sessions_from_store`.
const STORE_LIST_LOAD_CONCURRENCY: usize = 16;

/// Session metadata (Python `SDKSessionInfo`), returned by `list_sessions()`,
/// `get_session_info()` and their store-backed variants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SDKSessionInfo {
    /// Unique session identifier (UUID).
    pub session_id: String,
    /// Display title: custom title, last prompt, summary, or first prompt.
    pub summary: String,
    /// Last modified time in milliseconds since epoch.
    pub last_modified: i64,
    /// Transcript size in bytes (local JSONL / serialized store entries).
    #[serde(default)]
    pub file_size: Option<i64>,
    /// User-set custom title or AI-generated title.
    #[serde(default)]
    pub custom_title: Option<String>,
    /// First meaningful user prompt.
    #[serde(default)]
    pub first_prompt: Option<String>,
    /// Git branch at the end of the session.
    #[serde(default)]
    pub git_branch: Option<String>,
    /// Working directory for the session.
    #[serde(default)]
    pub cwd: Option<String>,
    /// User-set session tag.
    #[serde(default)]
    pub tag: Option<String>,
    /// Creation time (ms since epoch) from the first entry's timestamp.
    #[serde(default)]
    pub created_at: Option<i64>,
}

impl SDKSessionInfo {
    /// A row with only the required fields set (Python dataclass defaults).
    pub fn new(
        session_id: impl Into<String>,
        summary: impl Into<String>,
        last_modified: i64,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            summary: summary.into(),
            last_modified,
            file_size: None,
            custom_title: None,
            first_prompt: None,
            git_branch: None,
            cwd: None,
            tag: None,
            created_at: None,
        }
    }
}

/// Former name of [`SessionMessage`] for store-backed readers; local and store
/// readers now share one type.
pub type SDKSessionMessage = SessionMessage;

fn invalid_session_id(session_id: &str) -> ClaudeSDKError {
    ClaudeSDKError::Session(format!("Invalid session_id: {session_id}"))
}

/// `project_path` (canonical directory, default `.`) and its `project_key`.
fn project_path_and_key(directory: Option<&str>) -> (String, String) {
    let project_path = canonicalize_path(directory.unwrap_or("."));
    let project_key = sanitize_path(&project_path);
    (project_path, project_key)
}

fn main_key(project_key: &str, session_id: &str) -> SessionKey {
    SessionKey {
        project_key: project_key.to_string(),
        session_id: session_id.to_string(),
        subpath: None,
    }
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

/// Rename a session by appending a `custom-title` entry to the store.
pub async fn rename_session_via_store(
    session_store: &SessionStoreHandle,
    session_id: &str,
    title: &str,
    directory: Option<&str>,
) -> Result<()> {
    if !is_valid_uuid(session_id) {
        return Err(invalid_session_id(session_id));
    }
    let title = title.trim();
    if title.is_empty() {
        return Err(ClaudeSDKError::Session(
            "title must be non-empty".to_string(),
        ));
    }
    let (_, project_key) = project_path_and_key(directory);
    let entry = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": session_id,
        "uuid": uuid::Uuid::new_v4().to_string(),
        "timestamp": iso_now(),
    });
    session_store
        .append(main_key(&project_key, session_id), vec![into_entry(entry)])
        .await
}

/// Tag a session by appending a `tag` entry to the store. `None` clears it.
pub async fn tag_session_via_store(
    session_store: &SessionStoreHandle,
    session_id: &str,
    tag: Option<&str>,
    directory: Option<&str>,
) -> Result<()> {
    if !is_valid_uuid(session_id) {
        return Err(invalid_session_id(session_id));
    }
    let tag = tag.map(sanitize::sanitize_tag).transpose()?;
    let (_, project_key) = project_path_and_key(directory);
    let entry = serde_json::json!({
        "type": "tag",
        "tag": tag.unwrap_or_default(),
        "sessionId": session_id,
        "uuid": uuid::Uuid::new_v4().to_string(),
        "timestamp": iso_now(),
    });
    session_store
        .append(main_key(&project_key, session_id), vec![into_entry(entry)])
        .await
}

/// Delete a session from the store (a no-op for stores without `delete`).
pub async fn delete_session_via_store(
    session_store: &SessionStoreHandle,
    session_id: &str,
    directory: Option<&str>,
) -> Result<()> {
    if !is_valid_uuid(session_id) {
        return Err(invalid_session_id(session_id));
    }
    let (_, project_key) = project_path_and_key(directory);
    session_store
        .delete(main_key(&project_key, session_id))
        .await
}

fn into_entry(value: Value) -> SessionStoreEntry {
    match value {
        Value::Object(map) => map,
        _ => SessionStoreEntry::new(),
    }
}

// ---------------------------------------------------------------------------
// Listing / info
// ---------------------------------------------------------------------------

/// `_load_store_entries_as_jsonl`: `None` when the session has no entries.
async fn load_store_entries_as_jsonl(
    session_store: &SessionStoreHandle,
    project_key: &str,
    session_id: &str,
) -> Result<Option<String>> {
    let entries = session_store
        .load(main_key(project_key, session_id))
        .await?;
    Ok(entries
        .filter(|entries| !entries.is_empty())
        .map(|entries| lite::entries_to_jsonl(&entries)))
}

/// `_derive_infos_via_load`: bounded-concurrency `load()` + lite-parse per
/// listing row. Adapter errors degrade the row to an empty summary; sidechain
/// and no-summary sessions are dropped.
async fn derive_infos_via_load(
    session_store: &SessionStoreHandle,
    listing: &[SessionStoreListEntry],
    project_key: &str,
    project_path: &str,
) -> Vec<SDKSessionInfo> {
    let outcomes: Vec<Result<Option<String>>> = stream::iter(listing.iter())
        .map(|entry| load_store_entries_as_jsonl(session_store, project_key, &entry.session_id))
        .buffered(STORE_LIST_LOAD_CONCURRENCY)
        .collect()
        .await;

    let mut results = Vec::new();
    for (entry, outcome) in listing.iter().zip(outcomes) {
        match outcome {
            Err(_) => results.push(SDKSessionInfo::new(&entry.session_id, "", entry.mtime)),
            Ok(None) => {}
            Ok(Some(jsonl)) => {
                let lite = lite::jsonl_to_lite(&jsonl, entry.mtime);
                if let Some(mut parsed) =
                    lite::parse_session_info_from_lite(&entry.session_id, &lite, Some(project_path))
                {
                    parsed.last_modified = entry.mtime;
                    results.push(parsed);
                }
            }
        }
    }
    results
}

/// List sessions from a `SessionStore`, sorted by `last_modified` descending.
///
/// Uses `list_session_summaries()` when the store supports it (gap-filling
/// missing or stale summaries from `list_sessions()` + `load()` after
/// pagination); otherwise loads every session listed by `list_sessions()`.
/// Errors when the store supports neither.
pub async fn list_sessions_from_store(
    session_store: &SessionStoreHandle,
    directory: Option<&str>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SDKSessionInfo>> {
    let (project_path, project_key) = project_path_and_key(directory);
    let has_list_sessions = session_store.supports_list_sessions();

    if session_store.supports_list_session_summaries() {
        let summaries = session_store.list_session_summaries(&project_key).await?;
        let (listing, known_mtimes) = if has_list_sessions {
            let listing = session_store.list_sessions(&project_key).await?;
            let known = listing
                .iter()
                .map(|entry| (entry.session_id.clone(), entry.mtime))
                .collect::<std::collections::HashMap<_, _>>();
            (listing, known)
        } else {
            tracing::debug!(
                "list_session_summaries without list_sessions: gap-fill skipped; \
                 sessions lacking a sidecar will be omitted"
            );
            (Vec::new(), std::collections::HashMap::new())
        };

        struct Slot {
            mtime: i64,
            session_id: String,
            info: Option<SDKSessionInfo>,
        }
        let mut slots = Vec::new();
        let mut fresh_summary_ids = std::collections::HashSet::new();
        for summary in &summaries {
            if has_list_sessions {
                match known_mtimes.get(&summary.session_id) {
                    None => continue,
                    Some(known) if summary.mtime < *known => continue,
                    Some(_) => {}
                }
            }
            fresh_summary_ids.insert(summary.session_id.clone());
            if let Some(info) = summary_entry_to_sdk_info(summary, Some(&project_path)) {
                slots.push(Slot {
                    mtime: summary.mtime,
                    session_id: summary.session_id.clone(),
                    info: Some(info),
                });
            }
        }
        if has_list_sessions {
            slots.extend(
                listing
                    .iter()
                    .filter(|entry| !fresh_summary_ids.contains(&entry.session_id))
                    .map(|entry| Slot {
                        mtime: entry.mtime,
                        session_id: entry.session_id.clone(),
                        info: None,
                    }),
            );
        }

        slots.sort_by_key(|slot| std::cmp::Reverse(slot.mtime));
        let mut page = chain::page(slots, limit, offset);

        let to_fill: Vec<SessionStoreListEntry> = page
            .iter()
            .filter(|slot| slot.info.is_none())
            .map(|slot| SessionStoreListEntry {
                session_id: slot.session_id.clone(),
                mtime: slot.mtime,
            })
            .collect();
        if !to_fill.is_empty() {
            let mut filled: std::collections::HashMap<String, SDKSessionInfo> =
                derive_infos_via_load(session_store, &to_fill, &project_key, &project_path)
                    .await
                    .into_iter()
                    .map(|info| (info.session_id.clone(), info))
                    .collect();
            for slot in page.iter_mut().filter(|slot| slot.info.is_none()) {
                slot.info = filled.remove(&slot.session_id);
            }
        }
        return Ok(page.into_iter().filter_map(|slot| slot.info).collect());
    }

    if !has_list_sessions {
        return Err(ClaudeSDKError::Session(
            "session_store implements neither list_session_summaries() nor list_sessions() \
             -- cannot list sessions. Provide a store with at least one of those methods."
                .to_string(),
        ));
    }
    let listing = session_store.list_sessions(&project_key).await?;
    let results = derive_infos_via_load(session_store, &listing, &project_key, &project_path).await;
    Ok(apply_sort_limit_offset(results, limit, offset))
}

/// Read metadata for a single session from a `SessionStore` (lite-parse of
/// the serialized entries; `last_modified` from the last entry's timestamp).
pub async fn get_session_info_from_store(
    session_store: &SessionStoreHandle,
    session_id: &str,
    directory: Option<&str>,
) -> Result<Option<SDKSessionInfo>> {
    if !is_valid_uuid(session_id) {
        return Ok(None);
    }
    let (project_path, project_key) = project_path_and_key(directory);
    let Some(jsonl) = load_store_entries_as_jsonl(session_store, &project_key, session_id).await?
    else {
        return Ok(None);
    };
    let lite = lite::jsonl_to_lite(&jsonl, lite::mtime_from_jsonl_tail(&jsonl));
    Ok(lite::parse_session_info_from_lite(
        session_id,
        &lite,
        Some(&project_path),
    ))
}

// ---------------------------------------------------------------------------
// Messages / subagents
// ---------------------------------------------------------------------------

/// Read a session's conversation messages from a `SessionStore` (same chain
/// reconstruction as the local-disk reader).
pub async fn get_session_messages_from_store(
    session_store: &SessionStoreHandle,
    session_id: &str,
    directory: Option<&str>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SessionMessage>> {
    if !is_valid_uuid(session_id) {
        return Ok(Vec::new());
    }
    let (_, project_key) = project_path_and_key(directory);
    let Some(entries) = session_store
        .load(main_key(&project_key, session_id))
        .await?
        .filter(|entries| !entries.is_empty())
    else {
        return Ok(Vec::new());
    };
    let entries = chain::filter_transcript_entries(entries);
    Ok(chain::entries_to_session_messages(&entries, limit, offset))
}

/// List subagent IDs for a session from a `SessionStore`. Requires
/// `list_subkeys()` support.
pub async fn list_subagents_from_store(
    session_store: &SessionStoreHandle,
    session_id: &str,
    directory: Option<&str>,
) -> Result<Vec<String>> {
    if !is_valid_uuid(session_id) {
        return Ok(Vec::new());
    }
    if !session_store.supports_list_subkeys() {
        return Err(ClaudeSDKError::Session(
            "session_store does not implement list_subkeys() -- cannot list subagents. \
             Provide a store with a list_subkeys() method."
                .to_string(),
        ));
    }
    let (_, project_key) = project_path_and_key(directory);
    let subkeys = session_store
        .list_subkeys(SessionListSubkeysKey {
            project_key,
            session_id: session_id.to_string(),
        })
        .await?;
    let mut seen = std::collections::HashSet::new();
    let mut ids = Vec::new();
    for subkey in subkeys {
        if !subkey.starts_with("subagents/") {
            continue;
        }
        let last = subkey.rsplit('/').next().unwrap_or_default();
        if let Some(agent_id) = last.strip_prefix("agent-") {
            if seen.insert(agent_id.to_string()) {
                ids.push(agent_id.to_string());
            }
        }
    }
    Ok(ids)
}

/// Read a subagent's messages from a `SessionStore`. The subpath is found via
/// `list_subkeys()` when supported (else `subagents/agent-<id>` is tried);
/// `parent_tool_use_id` / `parent_agent_id` come from the subagent's last
/// `agent_metadata` entry.
pub async fn get_subagent_messages_from_store(
    session_store: &SessionStoreHandle,
    session_id: &str,
    agent_id: &str,
    directory: Option<&str>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SessionMessage>> {
    if !is_valid_uuid(session_id) || agent_id.is_empty() {
        return Ok(Vec::new());
    }
    let (_, project_key) = project_path_and_key(directory);

    let mut subpath = format!("subagents/agent-{agent_id}");
    if session_store.supports_list_subkeys() {
        let target = format!("agent-{agent_id}");
        let subkeys = session_store
            .list_subkeys(SessionListSubkeysKey {
                project_key: project_key.clone(),
                session_id: session_id.to_string(),
            })
            .await?;
        let Some(found) = subkeys.into_iter().find(|subkey| {
            subkey.starts_with("subagents/") && subkey.rsplit('/').next() == Some(target.as_str())
        }) else {
            return Ok(Vec::new());
        };
        subpath = found;
    }

    let Some(entries) = session_store
        .load(SessionKey {
            project_key,
            session_id: session_id.to_string(),
            subpath: Some(subpath),
        })
        .await?
        .filter(|entries| !entries.is_empty())
    else {
        return Ok(Vec::new());
    };

    let (metadata, transcript) = chain::split_agent_metadata(entries);
    if transcript.is_empty() {
        return Ok(Vec::new());
    }
    let (parent_tool_use_id, parent_agent_id) =
        chain::parent_ids_from_agent_metadata(metadata.as_ref());
    let transcript = chain::filter_transcript_entries(transcript);
    Ok(chain::entries_to_subagent_messages(
        &transcript,
        limit,
        offset,
        parent_tool_use_id.as_deref(),
        parent_agent_id.as_deref(),
    ))
}
