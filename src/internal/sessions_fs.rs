//! Session filesystem operations for Claude's on-disk JSONL transcripts.
//!
//! Ports the local-disk halves of upstream `_internal/sessions.py`
//! (`list_sessions`, `get_session_info`, `get_session_messages`,
//! `list_subagents`, `get_subagent_messages`) and
//! `_internal/session_mutations.py` (`rename_session`, `tag_session`,
//! `delete_session`).

use crate::error::{ClaudeSDKError, Result};
use crate::sessions::chain;
use crate::sessions::lite::{parse_session_info_from_lite, read_session_lite};
use crate::sessions::paths::{
    self, canonicalize_path, find_project_dir, get_worktree_paths, is_valid_uuid,
    probe_directory_candidates, project_dir_entries, sanitize_path, MAX_SANITIZED_LENGTH,
};
use crate::sessions::{py_json, sanitize, ListSessionsOptions, SDKSessionInfo, SessionMessage};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// list_sessions / get_session_info
// ---------------------------------------------------------------------------

/// `_read_sessions_from_dir`: lite-parse every `<uuid>.jsonl` in a project
/// dir. Unreadable files are skipped.
fn read_sessions_from_dir(project_dir: &Path, project_path: Option<&str>) -> Vec<SDKSessionInfo> {
    let Ok(entries) = std::fs::read_dir(project_dir) else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(session_id) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".jsonl"))
            .filter(|stem| is_valid_uuid(stem))
        else {
            continue;
        };
        let Some(lite) = read_session_lite(&entry.path()) else {
            continue;
        };
        if let Some(info) = parse_session_info_from_lite(session_id, &lite, project_path) {
            results.push(info);
        }
    }
    results
}

/// `_deduplicate_by_session_id`: keep the newest `last_modified` per id.
fn deduplicate_by_session_id(sessions: Vec<SDKSessionInfo>) -> Vec<SDKSessionInfo> {
    let mut order = Vec::new();
    let mut by_id: HashMap<String, SDKSessionInfo> = HashMap::new();
    for session in sessions {
        match by_id.get(&session.session_id) {
            None => {
                order.push(session.session_id.clone());
                by_id.insert(session.session_id.clone(), session);
            }
            Some(existing) if session.last_modified > existing.last_modified => {
                by_id.insert(session.session_id.clone(), session);
            }
            Some(_) => {}
        }
    }
    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// `_apply_sort_limit_offset`.
pub(crate) fn apply_sort_limit_offset(
    mut sessions: Vec<SDKSessionInfo>,
    limit: Option<usize>,
    offset: usize,
) -> Vec<SDKSessionInfo> {
    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_modified));
    chain::page(sessions, limit, offset)
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `_list_sessions_for_project`.
async fn list_sessions_for_project(
    directory: &str,
    limit: Option<usize>,
    offset: usize,
    include_worktrees: bool,
) -> Vec<SDKSessionInfo> {
    let canonical = canonicalize_path(directory);
    let worktree_paths = if include_worktrees {
        get_worktree_paths(&canonical).await
    } else {
        Vec::new()
    };

    let single_dir = || match find_project_dir(&canonical) {
        Some(project_dir) => apply_sort_limit_offset(
            read_sessions_from_dir(&project_dir, Some(&canonical)),
            limit,
            offset,
        ),
        None => Vec::new(),
    };
    if worktree_paths.len() <= 1 {
        return single_dir();
    }

    let case_insensitive = cfg!(windows);
    let fold = |name: &str| {
        if case_insensitive {
            name.to_lowercase()
        } else {
            name.to_string()
        }
    };
    let mut indexed: Vec<(String, String)> = worktree_paths
        .iter()
        .map(|worktree| (worktree.clone(), fold(&sanitize_path(worktree))))
        .collect();
    indexed.sort_by_key(|(_, prefix)| std::cmp::Reverse(prefix.len()));

    let Some(all_dirents) = project_dir_entries(true) else {
        return single_dir();
    };

    let mut all_sessions = Vec::new();
    let mut seen_dirs = HashSet::new();
    if let Some(project_dir) = find_project_dir(&canonical) {
        seen_dirs.insert(fold(&dir_name(&project_dir)));
        all_sessions.extend(read_sessions_from_dir(&project_dir, Some(&canonical)));
    }
    for entry in all_dirents {
        let name = fold(&dir_name(&entry));
        if seen_dirs.contains(&name) {
            continue;
        }
        for (worktree, prefix) in &indexed {
            let is_match = name == *prefix
                || (prefix.len() >= MAX_SANITIZED_LENGTH
                    && name.starts_with(&format!("{prefix}-")));
            if is_match {
                seen_dirs.insert(name.clone());
                all_sessions.extend(read_sessions_from_dir(&entry, Some(worktree)));
                break;
            }
        }
    }
    apply_sort_limit_offset(deduplicate_by_session_id(all_sessions), limit, offset)
}

/// `list_sessions`.
pub async fn list_sessions(opts: &ListSessionsOptions) -> Vec<SDKSessionInfo> {
    let offset = opts.offset.unwrap_or(0);
    if let Some(directory) = opts.directory.as_deref().filter(|dir| !dir.is_empty()) {
        return list_sessions_for_project(directory, opts.limit, offset, opts.include_worktrees)
            .await;
    }
    let Some(project_dirs) = project_dir_entries(true) else {
        return Vec::new();
    };
    let all_sessions = project_dirs
        .iter()
        .flat_map(|project_dir| read_sessions_from_dir(project_dir, None))
        .collect();
    apply_sort_limit_offset(deduplicate_by_session_id(all_sessions), opts.limit, offset)
}

/// `get_session_info`. The first candidate file that reads is parsed and
/// returned, even when the parse yields `None`.
pub async fn get_session_info(session_id: &str, directory: Option<&str>) -> Option<SDKSessionInfo> {
    if !is_valid_uuid(session_id) {
        return None;
    }
    let file_name = format!("{session_id}.jsonl");
    if let Some(directory) = directory.filter(|dir| !dir.is_empty()) {
        return probe_directory_candidates(directory, |project_dir, project_path| {
            Ok(read_session_lite(&project_dir.join(&file_name))
                .map(|lite| parse_session_info_from_lite(session_id, &lite, Some(project_path))))
        })
        .await
        .ok()
        .flatten()
        .flatten();
    }
    for project_dir in project_dir_entries(true)? {
        if let Some(lite) = read_session_lite(&project_dir.join(&file_name)) {
            return parse_session_info_from_lite(session_id, &lite, None);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// get_session_messages / subagents
// ---------------------------------------------------------------------------

fn read_non_empty(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// `_read_session_file`: content of the first non-empty candidate.
async fn read_session_file(session_id: &str, directory: Option<&str>) -> Option<String> {
    let file_name = format!("{session_id}.jsonl");
    if let Some(directory) = directory.filter(|dir| !dir.is_empty()) {
        return probe_directory_candidates(directory, |project_dir, _| {
            Ok(read_non_empty(&project_dir.join(&file_name)))
        })
        .await
        .ok()
        .flatten();
    }
    project_dir_entries(false)?
        .iter()
        .find_map(|project_dir| read_non_empty(&project_dir.join(&file_name)))
}

fn stat_non_empty(path: PathBuf) -> Option<PathBuf> {
    std::fs::metadata(&path)
        .ok()
        .filter(|meta| meta.len() > 0)
        .map(|_| path)
}

/// `_resolve_session_file_path`: path of the first non-empty candidate
/// (project dir, then other git worktrees; or every project without a
/// directory).
pub(crate) async fn resolve_session_file_path(
    session_id: &str,
    directory: Option<&str>,
) -> Option<PathBuf> {
    let file_name = format!("{session_id}.jsonl");
    if let Some(directory) = directory.filter(|dir| !dir.is_empty()) {
        return probe_directory_candidates(directory, |project_dir, _| {
            Ok(stat_non_empty(project_dir.join(&file_name)))
        })
        .await
        .ok()
        .flatten();
    }
    project_dir_entries(true)?
        .into_iter()
        .find_map(|project_dir| stat_non_empty(project_dir.join(&file_name)))
}

/// `get_session_messages`.
pub async fn get_session_messages(
    session_id: &str,
    directory: Option<&str>,
    limit: Option<usize>,
    offset: usize,
) -> Vec<SessionMessage> {
    if !is_valid_uuid(session_id) {
        return Vec::new();
    }
    let Some(content) = read_session_file(session_id, directory).await else {
        return Vec::new();
    };
    let entries = chain::parse_transcript_entries(&content);
    chain::entries_to_session_messages(&entries, limit, offset)
}

/// `_collect_agent_files`: recursively collect `agent-*.jsonl` files.
fn collect_agent_files(base_dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, output: &mut Vec<(String, PathBuf)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_file() {
                if let Some(agent_id) = name
                    .strip_prefix("agent-")
                    .and_then(|rest| rest.strip_suffix(".jsonl"))
                {
                    output.push((agent_id.to_string(), path));
                }
            } else if path.is_dir() {
                walk(&path, output);
            }
        }
    }
    let mut output = Vec::new();
    walk(base_dir, &mut output);
    output
}

/// `_resolve_subagents_dir`.
async fn resolve_subagents_dir(session_id: &str, directory: Option<&str>) -> Option<PathBuf> {
    let resolved = resolve_session_file_path(session_id, directory).await?;
    Some(resolved.with_extension("").join("subagents"))
}

/// `list_subagents`.
pub async fn list_subagents(session_id: &str, directory: Option<&str>) -> Vec<String> {
    if !is_valid_uuid(session_id) {
        return Vec::new();
    }
    let Some(subagents_dir) = resolve_subagents_dir(session_id, directory).await else {
        return Vec::new();
    };
    collect_agent_files(&subagents_dir)
        .into_iter()
        .map(|(agent_id, _)| agent_id)
        .collect()
}

/// `get_subagent_messages`.
pub async fn get_subagent_messages(
    session_id: &str,
    agent_id: &str,
    directory: Option<&str>,
    limit: Option<usize>,
    offset: usize,
) -> Vec<SessionMessage> {
    if !is_valid_uuid(session_id) || agent_id.is_empty() {
        return Vec::new();
    }
    let Some(subagents_dir) = resolve_subagents_dir(session_id, directory).await else {
        return Vec::new();
    };
    let Some((_, agent_file)) = collect_agent_files(&subagents_dir)
        .into_iter()
        .find(|(found_id, _)| found_id == agent_id)
    else {
        return Vec::new();
    };
    let Some(content) = read_non_empty(&agent_file) else {
        return Vec::new();
    };
    let meta = chain::read_agent_metadata_sidecar(&agent_file)
        .ok()
        .flatten();
    let (parent_tool_use_id, parent_agent_id) =
        chain::parent_ids_from_agent_metadata(meta.as_ref());
    let entries = chain::parse_transcript_entries(&content);
    chain::entries_to_subagent_messages(
        &entries,
        limit,
        offset,
        parent_tool_use_id.as_deref(),
        parent_agent_id.as_deref(),
    )
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

fn not_found(message: String) -> ClaudeSDKError {
    ClaudeSDKError::IO(std::io::Error::new(std::io::ErrorKind::NotFound, message))
}

fn invalid_session_id(session_id: &str) -> ClaudeSDKError {
    ClaudeSDKError::Session(format!("Invalid session_id: {session_id}"))
}

/// `_try_append`: `Ok(false)` when the file is missing (ENOENT/ENOTDIR) or
/// empty, so the search continues; other errors propagate.
fn try_append(path: &Path, data: &[u8]) -> Result<bool> {
    use std::io::Write;

    let mut file = match std::fs::OpenOptions::new().append(true).open(path) {
        Ok(file) => file,
        Err(error) if is_missing(&error) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.write_all(data)?;
    Ok(true)
}

fn is_missing(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
        || error.kind() == std::io::ErrorKind::NotADirectory
}

/// `_append_to_session`.
async fn append_to_session(session_id: &str, data: &str, directory: Option<&str>) -> Result<()> {
    let file_name = format!("{session_id}.jsonl");
    if let Some(directory) = directory.filter(|dir| !dir.is_empty()) {
        let appended = probe_directory_candidates(directory, |project_dir, _| {
            Ok(try_append(&project_dir.join(&file_name), data.as_bytes())?.then_some(()))
        })
        .await?;
        return appended.ok_or_else(|| {
            not_found(format!(
                "Session {session_id} not found in project directory for {directory}"
            ))
        });
    }
    let Ok(entries) = std::fs::read_dir(paths::projects_dir(None)) else {
        return Err(not_found(format!(
            "Session {session_id} not found (no projects directory)"
        )));
    };
    for entry in entries.flatten() {
        if try_append(&entry.path().join(&file_name), data.as_bytes())? {
            return Ok(());
        }
    }
    Err(not_found(format!(
        "Session {session_id} not found in any project directory"
    )))
}

/// `rename_session`: appends `{"type":"custom-title","customTitle":...,"sessionId":...}`.
pub async fn rename_session(session_id: &str, title: &str, directory: Option<&str>) -> Result<()> {
    if !is_valid_uuid(session_id) {
        return Err(invalid_session_id(session_id));
    }
    let title = title.trim();
    if title.is_empty() {
        return Err(ClaudeSDKError::Session(
            "title must be non-empty".to_string(),
        ));
    }
    let line = custom_title_line(session_id, title);
    append_to_session(session_id, &line, directory).await
}

/// `tag_session`: appends `{"type":"tag","tag":...,"sessionId":...}`; `None`
/// writes an empty tag, which readers treat as cleared.
pub async fn tag_session(
    session_id: &str,
    tag: Option<&str>,
    directory: Option<&str>,
) -> Result<()> {
    if !is_valid_uuid(session_id) {
        return Err(invalid_session_id(session_id));
    }
    let tag = tag.map(sanitize::sanitize_tag).transpose()?;
    let line = tag_line(session_id, tag.as_deref().unwrap_or_default());
    append_to_session(session_id, &line, directory).await
}

/// The exact line Python's `rename_session` appends (with trailing newline).
pub(crate) fn custom_title_line(session_id: &str, title: &str) -> String {
    let entry = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": session_id,
    });
    format!("{}\n", py_json::dumps_compact(&entry))
}

/// The exact line Python's `tag_session` appends (with trailing newline).
pub(crate) fn tag_line(session_id: &str, tag: &str) -> String {
    let entry = serde_json::json!({
        "type": "tag",
        "tag": tag,
        "sessionId": session_id,
    });
    format!("{}\n", py_json::dumps_compact(&entry))
}

/// `_find_session_file_with_dir`: (non-empty file, its project dir).
pub(crate) async fn find_session_file_with_dir(
    session_id: &str,
    directory: Option<&str>,
) -> Option<(PathBuf, PathBuf)> {
    let file_name = format!("{session_id}.jsonl");
    let try_dir = |project_dir: &Path| {
        stat_non_empty(project_dir.join(&file_name)).map(|path| (path, project_dir.to_path_buf()))
    };
    if let Some(directory) = directory.filter(|dir| !dir.is_empty()) {
        return probe_directory_candidates(directory, |project_dir, _| Ok(try_dir(project_dir)))
            .await
            .ok()
            .flatten();
    }
    project_dir_entries(false)?
        .iter()
        .find_map(|project_dir| try_dir(project_dir))
}

/// `delete_session`: removes the transcript (error when missing) and,
/// best-effort, the sibling `<session_id>/` subagent directory.
pub async fn delete_session(session_id: &str, directory: Option<&str>) -> Result<()> {
    if !is_valid_uuid(session_id) {
        return Err(invalid_session_id(session_id));
    }
    let Some((path, _)) = find_session_file_with_dir(session_id, directory).await else {
        let suffix = directory
            .filter(|dir| !dir.is_empty())
            .map(|dir| format!(" in project directory for {dir}"))
            .unwrap_or_default();
        return Err(not_found(format!("Session {session_id} not found{suffix}")));
    };
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(not_found(format!("Session {session_id} not found")));
        }
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::remove_dir_all(parent.join(session_id)).await;
    }
    Ok(())
}
