//! Transcript chain reconstruction shared by the local-disk and
//! `SessionStore` message readers. Ports `_parse_transcript_entries`,
//! `_filter_transcript_entries`, `_build_conversation_chain`,
//! `_recover_orphaned_parallel_tool_results` (#1422), `_build_subagent_chain`
//! and the agent-metadata helpers (#1207) from upstream `_internal/sessions.py`.

use crate::sessions::SessionMessage;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub(crate) type Entry = Map<String, Value>;

/// Transcript entry types that carry `uuid` + `parentUuid` chain links.
pub(crate) const TRANSCRIPT_ENTRY_TYPES: [&str; 5] =
    ["user", "assistant", "progress", "system", "attachment"];

/// Python truthiness of an optional JSON value.
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

fn entry_type(entry: &Entry) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

fn uuid_of(entry: &Entry) -> &str {
    entry
        .get("uuid")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// A truthy string `parentUuid`.
fn parent_of(entry: &Entry) -> Option<&str> {
    entry
        .get("parentUuid")
        .and_then(Value::as_str)
        .filter(|parent| !parent.is_empty())
}

fn is_transcript_entry(entry: &Entry) -> bool {
    entry_type(entry).is_some_and(|ty| TRANSCRIPT_ENTRY_TYPES.contains(&ty))
        && entry.get("uuid").is_some_and(Value::is_string)
}

/// `_parse_transcript_entries`: JSON-object lines of a transcript type with a
/// string `uuid`; corrupt lines are skipped.
pub(crate) fn parse_transcript_entries(content: &str) -> Vec<Entry> {
    content
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(entry)) if is_transcript_entry(&entry) => Some(entry),
            _ => None,
        })
        .collect()
}

/// `_filter_transcript_entries`: the already-parsed (store) counterpart of
/// [`parse_transcript_entries`].
pub(crate) fn filter_transcript_entries(entries: Vec<Entry>) -> Vec<Entry> {
    entries.into_iter().filter(is_transcript_entry).collect()
}

/// Python `dict` keyed by uuid: last value wins, first-insertion order kept.
struct UuidIndex<'a> {
    order: Vec<&'a str>,
    by_uuid: HashMap<&'a str, usize>,
}

impl<'a> UuidIndex<'a> {
    fn new(entries: &'a [Entry]) -> Self {
        let mut order = Vec::new();
        let mut by_uuid = HashMap::new();
        for (idx, entry) in entries.iter().enumerate() {
            let uuid = uuid_of(entry);
            if by_uuid.insert(uuid, idx).is_none() {
                order.push(uuid);
            }
        }
        Self { order, by_uuid }
    }

    fn get(&self, uuid: &str) -> Option<usize> {
        self.by_uuid.get(uuid).copied()
    }

    /// `by_uuid.values()` in dict order.
    fn values(&self) -> impl Iterator<Item = usize> + '_ {
        self.order.iter().map(|uuid| self.by_uuid[uuid])
    }
}

/// `_build_conversation_chain`: indices into `entries`, root -> leaf, with
/// orphaned parallel tool calls/results restored.
pub(crate) fn build_conversation_chain(entries: &[Entry]) -> Vec<usize> {
    if entries.is_empty() {
        return Vec::new();
    }
    let index = UuidIndex::new(entries);
    // Last file position of each uuid (tie-breaking for leaf selection).
    let mut entry_index: HashMap<&str, usize> = HashMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        entry_index.insert(uuid_of(entry), idx);
    }
    let parent_uuids: HashSet<&str> = entries.iter().filter_map(parent_of).collect();

    let mut leaves = Vec::new();
    for (terminal_idx, terminal) in entries.iter().enumerate() {
        if parent_uuids.contains(uuid_of(terminal)) {
            continue;
        }
        let mut current = Some(terminal_idx);
        let mut walk_seen = HashSet::new();
        while let Some(idx) = current {
            let entry = &entries[idx];
            if !walk_seen.insert(uuid_of(entry)) {
                break;
            }
            if matches!(entry_type(entry), Some("user" | "assistant")) {
                leaves.push(idx);
                break;
            }
            current = parent_of(entry).and_then(|parent| index.get(parent));
        }
    }
    if leaves.is_empty() {
        return Vec::new();
    }

    let pick_best = |candidates: &[usize]| -> usize {
        let mut best = candidates[0];
        let mut best_idx = entry_index[uuid_of(&entries[best])];
        for &candidate in &candidates[1..] {
            let candidate_idx = entry_index[uuid_of(&entries[candidate])];
            if candidate_idx > best_idx {
                best = candidate;
                best_idx = candidate_idx;
            }
        }
        best
    };
    let main_leaves: Vec<usize> = leaves
        .iter()
        .copied()
        .filter(|&idx| {
            let leaf = &entries[idx];
            !truthy(leaf.get("isSidechain"))
                && !truthy(leaf.get("teamName"))
                && !truthy(leaf.get("isMeta"))
        })
        .collect();
    let leaf = if main_leaves.is_empty() {
        pick_best(&leaves)
    } else {
        pick_best(&main_leaves)
    };

    let mut chain = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut current = Some(leaf);
    while let Some(idx) = current {
        let entry = &entries[idx];
        if !seen.insert(uuid_of(entry).to_string()) {
            break;
        }
        chain.push(idx);
        current = parent_of(entry).and_then(|parent| index.get(parent));
    }
    chain.reverse();
    recover_orphaned_parallel_tool_results(entries, &index, chain, &mut seen)
}

/// `_assistant_message_id`: the API `message.id` of an assistant entry.
fn assistant_message_id(entry: &Entry) -> Option<&str> {
    if entry_type(entry) != Some("assistant") {
        return None;
    }
    entry
        .get("message")?
        .as_object()?
        .get("id")?
        .as_str()
        .filter(|id| !id.is_empty())
}

/// `_content_block_ids`.
fn content_block_ids<'a>(entry: &'a Entry, block_type: &str, id_field: &str) -> Vec<&'a str> {
    let Some(content) = entry
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    content
        .iter()
        .filter_map(Value::as_object)
        .filter(|block| block.get("type").and_then(Value::as_str) == Some(block_type))
        .filter_map(|block| block.get(id_field).and_then(Value::as_str))
        .collect()
}

/// `_is_tool_result_entry`.
fn is_tool_result_entry(entry: &Entry) -> bool {
    if entry_type(entry) != Some("user") || parent_of(entry).is_none() {
        return false;
    }
    entry
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|content| {
            content.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_result")
                    && block.is_object()
            })
        })
}

fn none_if_null(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

/// `_same_writer`.
fn same_writer(a: &Entry, b: &Entry) -> bool {
    truthy(a.get("isSidechain")) == truthy(b.get("isSidechain"))
        && none_if_null(a.get("agentId")) == none_if_null(b.get("agentId"))
}

fn timestamp_key(entry: &Entry) -> &str {
    entry
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// `_recover_orphaned_parallel_tool_results` (#1422): puts back the parallel
/// tool calls and results a `parentUuid` walk leaves off the chain. `seen`
/// holds the chain's uuids; recovered uuids are added to it.
fn recover_orphaned_parallel_tool_results(
    entries: &[Entry],
    index: &UuidIndex<'_>,
    chain: Vec<usize>,
    seen: &mut HashSet<String>,
) -> Vec<usize> {
    let chain_assistants: Vec<usize> = chain
        .iter()
        .copied()
        .filter(|&idx| entry_type(&entries[idx]) == Some("assistant"))
        .collect();
    if chain_assistants.is_empty() {
        return chain;
    }

    let mut anchor_by_message_id: HashMap<&str, usize> = HashMap::new();
    for &idx in &chain_assistants {
        if let Some(message_id) = assistant_message_id(&entries[idx]) {
            anchor_by_message_id.insert(message_id, idx);
        }
    }
    if anchor_by_message_id.is_empty() {
        return chain;
    }

    let mut siblings_by_message_id: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut call_owner: HashMap<&str, Option<usize>> = HashMap::new();
    let mut tool_result_entries = Vec::new();
    for idx in index.values() {
        let entry = &entries[idx];
        if let Some(message_id) = assistant_message_id(entry) {
            siblings_by_message_id
                .entry(message_id)
                .or_default()
                .push(idx);
            for call_id in content_block_ids(entry, "tool_use", "id") {
                match call_owner.get(call_id) {
                    None => {
                        call_owner.insert(call_id, Some(idx));
                    }
                    Some(owner) => {
                        let same_response = owner.is_some_and(|owner| {
                            assistant_message_id(&entries[owner]) == Some(message_id)
                        });
                        call_owner.insert(call_id, same_response.then_some(idx));
                    }
                }
            }
        } else if is_tool_result_entry(entry) {
            tool_result_entries.push(idx);
        }
    }

    let mut results_by_assistant: HashMap<&str, Vec<usize>> = HashMap::new();
    for &result_idx in &tool_result_entries {
        let result = &entries[result_idx];
        if let Some(parent) = parent_of(result) {
            results_by_assistant
                .entry(parent)
                .or_default()
                .push(result_idx);
        }
        if let Some(source) = result
            .get("sourceToolAssistantUUID")
            .and_then(Value::as_str)
        {
            if let Some(source_idx) = index.get(source) {
                if same_writer(result, &entries[source_idx]) {
                    results_by_assistant
                        .entry(source)
                        .or_default()
                        .push(result_idx);
                }
            }
        }
        for call_id in content_block_ids(result, "tool_result", "tool_use_id") {
            if let Some(Some(owner)) = call_owner.get(call_id) {
                if same_writer(result, &entries[*owner]) {
                    results_by_assistant
                        .entry(uuid_of(&entries[*owner]))
                        .or_default()
                        .push(result_idx);
                }
            }
        }
    }

    let mut answered_on_chain: Option<HashSet<&str>> = None;
    let mut file_position: Option<HashMap<&str, usize>> = None;
    let mut processed: HashSet<&str> = HashSet::new();
    let mut inserts: HashMap<&str, Vec<usize>> = HashMap::new();

    for &assistant in &chain_assistants {
        let Some(message_id) = assistant_message_id(&entries[assistant]) else {
            continue;
        };
        if !processed.insert(message_id) {
            continue;
        }
        let group = siblings_by_message_id
            .get(message_id)
            .cloned()
            .unwrap_or_else(|| vec![assistant]);
        let member_uuids: HashSet<&str> = group.iter().map(|&idx| uuid_of(&entries[idx])).collect();
        let mut orphaned_siblings: Vec<usize> = group
            .iter()
            .copied()
            .filter(|&idx| !seen.contains(uuid_of(&entries[idx])))
            .collect();
        let mut orphaned_results = Vec::new();
        let mut strays = Vec::new();
        let mut collected: HashSet<&str> = HashSet::new();
        for &member in &group {
            let Some(results) = results_by_assistant.get(uuid_of(&entries[member])) else {
                continue;
            };
            for &result_idx in results {
                let result_uuid = uuid_of(&entries[result_idx]);
                if seen.contains(result_uuid) || !collected.insert(result_uuid) {
                    continue;
                }
                if parent_of(&entries[result_idx])
                    .is_some_and(|parent| member_uuids.contains(parent))
                {
                    orphaned_results.push(result_idx);
                } else {
                    strays.push(result_idx);
                }
            }
        }

        if !strays.is_empty() {
            let answered_on_chain = answered_on_chain.get_or_insert_with(|| {
                chain
                    .iter()
                    .flat_map(|&idx| content_block_ids(&entries[idx], "tool_result", "tool_use_id"))
                    .collect()
            });
            let positions = file_position.get_or_insert_with(|| {
                index
                    .order
                    .iter()
                    .enumerate()
                    .map(|(position, uuid)| (*uuid, position))
                    .collect()
            });
            let mut answered = answered_on_chain.clone();
            for &result_idx in &orphaned_results {
                answered.extend(content_block_ids(
                    &entries[result_idx],
                    "tool_result",
                    "tool_use_id",
                ));
            }
            strays.sort_by_key(|&idx| positions[uuid_of(&entries[idx])]);
            for result_idx in strays {
                let call_ids =
                    content_block_ids(&entries[result_idx], "tool_result", "tool_use_id");
                let answers_open_call = call_ids.iter().any(|call_id| {
                    !answered.contains(call_id)
                        && call_owner.get(call_id).is_some_and(|owner| {
                            owner.is_some_and(|owner| {
                                member_uuids.contains(uuid_of(&entries[owner]))
                            })
                        })
                });
                if !answers_open_call {
                    continue;
                }
                answered.extend(call_ids);
                orphaned_results.push(result_idx);
            }
        }

        if orphaned_siblings.is_empty() && orphaned_results.is_empty() {
            continue;
        }
        orphaned_siblings
            .sort_by(|a, b| timestamp_key(&entries[*a]).cmp(timestamp_key(&entries[*b])));
        orphaned_results
            .sort_by(|a, b| timestamp_key(&entries[*a]).cmp(timestamp_key(&entries[*b])));
        let mut recovered = orphaned_siblings;
        recovered.extend(orphaned_results);
        for &idx in &recovered {
            seen.insert(uuid_of(&entries[idx]).to_string());
        }
        inserts.insert(
            uuid_of(&entries[anchor_by_message_id[message_id]]),
            recovered,
        );
    }

    if inserts.is_empty() {
        return chain;
    }
    let mut result_chain = Vec::with_capacity(chain.len());
    for idx in chain {
        result_chain.push(idx);
        if let Some(recovered) = inserts.get(uuid_of(&entries[idx])) {
            result_chain.extend(recovered.iter().copied());
        }
    }
    result_chain
}

/// `_is_visible_message`.
pub(crate) fn is_visible_message(entry: &Entry) -> bool {
    matches!(entry_type(entry), Some("user" | "assistant"))
        && !truthy(entry.get("isMeta"))
        && !truthy(entry.get("isSidechain"))
        && !truthy(entry.get("teamName"))
}

/// `_to_session_message`.
pub(crate) fn to_session_message(
    entry: &Entry,
    parent_tool_use_id: Option<&str>,
    parent_agent_id: Option<&str>,
) -> SessionMessage {
    let message_type = if entry_type(entry) == Some("user") {
        "user"
    } else {
        "assistant"
    };
    SessionMessage {
        r#type: message_type.to_string(),
        uuid: uuid_of(entry).to_string(),
        session_id: entry
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        message: entry.get("message").cloned().unwrap_or(Value::Null),
        parent_tool_use_id: parent_tool_use_id.map(String::from),
        parent_agent_id: parent_agent_id.map(String::from),
    }
}

/// Upstream paging: `limit > 0` -> `[offset:offset+limit]`, else `[offset:]`.
pub(crate) fn page<T>(items: Vec<T>, limit: Option<usize>, offset: usize) -> Vec<T> {
    let iter = items.into_iter().skip(offset);
    match limit.filter(|limit| *limit > 0) {
        Some(limit) => iter.take(limit).collect(),
        None => iter.collect(),
    }
}

/// `_entries_to_session_messages`: chain + visibility filter + paging.
pub(crate) fn entries_to_session_messages(
    entries: &[Entry],
    limit: Option<usize>,
    offset: usize,
) -> Vec<SessionMessage> {
    let messages = build_conversation_chain(entries)
        .into_iter()
        .map(|idx| &entries[idx])
        .filter(|entry| is_visible_message(entry))
        .map(|entry| to_session_message(entry, None, None))
        .collect();
    page(messages, limit, offset)
}

/// `_build_subagent_chain`: leaf = last user/assistant entry; walk parents.
pub(crate) fn build_subagent_chain(entries: &[Entry]) -> Vec<usize> {
    let index = UuidIndex::new(entries);
    let Some(leaf) = entries
        .iter()
        .rposition(|entry| matches!(entry_type(entry), Some("user" | "assistant")))
    else {
        return Vec::new();
    };
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut current = Some(leaf);
    while let Some(idx) = current {
        let entry = &entries[idx];
        if !seen.insert(uuid_of(entry)) {
            break;
        }
        chain.push(idx);
        current = parent_of(entry).and_then(|parent| index.get(parent));
    }
    chain.reverse();
    chain
}

/// `_entries_to_subagent_messages`: subagent chain (type filter only) with the
/// shared parent ids stamped on every message.
pub(crate) fn entries_to_subagent_messages(
    entries: &[Entry],
    limit: Option<usize>,
    offset: usize,
    parent_tool_use_id: Option<&str>,
    parent_agent_id: Option<&str>,
) -> Vec<SessionMessage> {
    let messages = build_subagent_chain(entries)
        .into_iter()
        .map(|idx| &entries[idx])
        .filter(|entry| matches!(entry_type(entry), Some("user" | "assistant")))
        .map(|entry| to_session_message(entry, parent_tool_use_id, parent_agent_id))
        .collect();
    page(messages, limit, offset)
}

/// `_agent_metadata_sidecar_path`: `agent-<id>.jsonl` -> `agent-<id>.meta.json`.
pub(crate) fn agent_metadata_sidecar_path(transcript_path: &Path) -> PathBuf {
    let name = transcript_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.strip_suffix(".jsonl").unwrap_or(&name);
    transcript_path.with_file_name(format!("{stem}.meta.json"))
}

/// `_read_agent_metadata_sidecar`: `Ok(None)` when the sidecar is missing,
/// not valid JSON or not an object; other IO errors propagate.
pub(crate) fn read_agent_metadata_sidecar(
    transcript_path: &Path,
) -> std::io::Result<Option<Entry>> {
    let bytes = match std::fs::read(agent_metadata_sidecar_path(transcript_path)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    Ok(match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(meta)) => Some(meta),
        _ => None,
    })
}

/// `_split_agent_metadata`: (last `agent_metadata` entry, remaining entries).
pub(crate) fn split_agent_metadata(entries: Vec<Entry>) -> (Option<Entry>, Vec<Entry>) {
    let mut metadata = None;
    let mut transcript = Vec::new();
    for entry in entries {
        if entry_type(&entry) == Some("agent_metadata") {
            metadata = Some(entry);
        } else {
            transcript.push(entry);
        }
    }
    (metadata, transcript)
}

/// `_parent_ids_from_agent_metadata`: (`toolUseId`, `parentAgentId`).
pub(crate) fn parent_ids_from_agent_metadata(
    meta: Option<&Entry>,
) -> (Option<String>, Option<String>) {
    let Some(meta) = meta.filter(|meta| !meta.is_empty()) else {
        return (None, None);
    };
    let field = |key: &str| meta.get(key).and_then(Value::as_str).map(String::from);
    (field("toolUseId"), field("parentAgentId"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entries(values: Vec<Value>) -> Vec<Entry> {
        values
            .into_iter()
            .map(|value| value.as_object().unwrap().clone())
            .collect()
    }

    fn uuids(entries: &[Entry], chain: &[usize]) -> Vec<String> {
        chain
            .iter()
            .map(|&idx| uuid_of(&entries[idx]).to_string())
            .collect()
    }

    #[test]
    fn picks_latest_main_leaf_and_ignores_logical_parent() {
        let entries = entries(vec![
            json!({"type": "user", "uuid": "u1"}),
            json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1"}),
            json!({"type": "user", "uuid": "side", "parentUuid": "a1", "isSidechain": true}),
            json!({"type": "system", "uuid": "c1", "parentUuid": null, "logicalParentUuid": "a1"}),
            json!({"type": "user", "uuid": "u2", "parentUuid": "c1"}),
        ]);
        assert_eq!(
            uuids(&entries, &build_conversation_chain(&entries)),
            ["c1", "u2"]
        );
    }

    #[test]
    fn recovers_parallel_tool_calls_and_results() {
        // One response (msg_1) with three parallel tool calls; each result
        // hangs off its own call, and the next turn continues from r3.
        let entries = entries(vec![
            json!({"type": "user", "uuid": "u1", "timestamp": "t0"}),
            json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1", "timestamp": "t1",
                   "message": {"id": "msg_1", "content": [{"type": "tool_use", "id": "c1"}]}}),
            json!({"type": "assistant", "uuid": "a2", "parentUuid": "a1", "timestamp": "t2",
                   "message": {"id": "msg_1", "content": [{"type": "tool_use", "id": "c2"}]}}),
            json!({"type": "assistant", "uuid": "a3", "parentUuid": "a2", "timestamp": "t3",
                   "message": {"id": "msg_1", "content": [{"type": "tool_use", "id": "c3"}]}}),
            json!({"type": "user", "uuid": "r1", "parentUuid": "a1", "timestamp": "t4",
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "c1"}]}}),
            json!({"type": "user", "uuid": "r2", "parentUuid": "a2", "timestamp": "t5",
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "c2"}]}}),
            json!({"type": "user", "uuid": "r3", "parentUuid": "a3", "timestamp": "t6",
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "c3"}]}}),
            json!({"type": "assistant", "uuid": "a4", "parentUuid": "r1", "timestamp": "t7",
                   "message": {"id": "msg_2", "content": [{"type": "text", "text": "done"}]}}),
        ]);
        // The walk from a4 reaches r1 -> a1 -> u1 only; the rest of the
        // response and its results are inserted right after the anchor a1.
        assert_eq!(
            uuids(&entries, &build_conversation_chain(&entries)),
            ["u1", "a1", "a2", "a3", "r2", "r3", "r1", "a4"]
        );
    }

    #[test]
    fn subagent_chain_uses_last_entry_and_type_filter() {
        let entries = entries(vec![
            json!({"type": "user", "uuid": "s1", "isSidechain": true}),
            json!({"type": "progress", "uuid": "p1", "parentUuid": "s1"}),
            json!({"type": "assistant", "uuid": "s2", "parentUuid": "p1", "isSidechain": true}),
        ]);
        let messages = entries_to_subagent_messages(&entries, None, 0, Some("toolu_1"), None);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].parent_tool_use_id.as_deref(), Some("toolu_1"));
    }
}
