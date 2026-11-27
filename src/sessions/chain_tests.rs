//! Ports of upstream `tests/test_sessions.py::TestBuildConversationChain`.

use super::chain::{build_conversation_chain, Entry};
use serde_json::{json, Value};

fn obj(value: Value) -> Entry {
    value.as_object().unwrap().clone()
}

/// `_parallel_tool_turn`: one response with `tools` parallel calls, results
/// written in `result_order`.
fn parallel_turn(parent: &str, tools: usize, result_order: &[usize]) -> (Vec<Entry>, Vec<Entry>) {
    let mut calls: Vec<Entry> = Vec::new();
    for i in 0..tools {
        let parent_uuid = calls
            .last()
            .map(|call| call["uuid"].clone())
            .unwrap_or(json!(parent));
        calls.push(obj(json!({
            "type": "assistant", "uuid": format!("call-{i}"), "parentUuid": parent_uuid,
            "sessionId": "s", "timestamp": format!("2026-01-01T00:00:01.{i:03}Z"),
            "message": {"role": "assistant", "id": "msg_parallel",
                        "content": [{"type": "tool_use", "id": format!("toolu_{i}"), "name": "Read", "input": {}}]}
        })));
    }
    let results = result_order
        .iter()
        .enumerate()
        .map(|(position, i)| {
            obj(json!({
                "type": "user", "uuid": format!("result-{i}"), "parentUuid": format!("call-{i}"),
                "sessionId": "s", "sourceToolAssistantUUID": format!("call-{i}"),
                "timestamp": format!("2026-01-01T00:00:02.{position:03}Z"),
                "message": {"role": "user",
                            "content": [{"type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": "ok"}]}
            }))
        })
        .collect();
    (calls, results)
}

fn prompt() -> Entry {
    obj(json!({"type": "user", "uuid": "prompt", "parentUuid": null}))
}

fn answer(parent: &str) -> Entry {
    obj(json!({"type": "assistant", "uuid": "answer", "parentUuid": parent}))
}

fn chain_uuids(entries: &[Entry]) -> Vec<String> {
    build_conversation_chain(entries)
        .into_iter()
        .map(|idx| entries[idx]["uuid"].as_str().unwrap().to_string())
        .collect()
}

fn turn(calls: Vec<Entry>, results: Vec<Entry>, tail: Vec<Entry>) -> Vec<Entry> {
    let mut entries = vec![prompt()];
    entries.extend(calls);
    entries.extend(results);
    entries.extend(tail);
    entries
}

#[test]
fn empty_single_linear_and_progress_only() {
    assert!(build_conversation_chain(&[]).is_empty());
    let single = vec![obj(
        json!({"type": "user", "uuid": "a", "parentUuid": null}),
    )];
    assert_eq!(chain_uuids(&single), ["a"]);
    let linear = vec![
        obj(json!({"type": "user", "uuid": "a", "parentUuid": null})),
        obj(json!({"type": "assistant", "uuid": "b", "parentUuid": "a"})),
        obj(json!({"type": "user", "uuid": "c", "parentUuid": "b"})),
    ];
    assert_eq!(chain_uuids(&linear), ["a", "b", "c"]);
    let progress = vec![
        obj(json!({"type": "progress", "uuid": "a", "parentUuid": null})),
        obj(json!({"type": "progress", "uuid": "b", "parentUuid": "a"})),
    ];
    assert!(chain_uuids(&progress).is_empty());
}

#[test]
fn recovers_sibling_calls_dropped_by_the_walk() {
    let (calls, results) = parallel_turn("prompt", 3, &[1, 2, 0]);
    let entries = turn(calls, results, vec![answer("result-0")]);
    assert_eq!(
        chain_uuids(&entries),
        ["prompt", "call-0", "call-1", "call-2", "result-1", "result-2", "result-0", "answer"]
    );
}

#[test]
fn recovered_results_are_in_completion_order() {
    let (calls, results) = parallel_turn("prompt", 3, &[1, 0, 2]);
    let entries = turn(calls, results, vec![answer("result-2")]);
    assert_eq!(
        chain_uuids(&entries),
        ["prompt", "call-0", "call-1", "call-2", "result-1", "result-0", "result-2", "answer"]
    );
}

#[test]
fn recovered_calls_are_in_timestamp_order() {
    let (calls, results) = parallel_turn("prompt", 3, &[1, 2, 0]);
    let reordered = vec![calls[0].clone(), calls[2].clone(), calls[1].clone()];
    let entries = turn(reordered, results, vec![answer("result-0")]);
    assert_eq!(chain_uuids(&entries)[1..4], ["call-0", "call-1", "call-2"]);
}

#[test]
fn sequential_tool_calls_are_unchanged() {
    let entries = vec![
        prompt(),
        obj(
            json!({"type": "assistant", "uuid": "call-0", "parentUuid": "prompt",
                   "message": {"id": "msg_1", "content": [{"type": "tool_use", "id": "toolu_0"}]}}),
        ),
        obj(
            json!({"type": "user", "uuid": "result-0", "parentUuid": "call-0",
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_0"}]}}),
        ),
        obj(
            json!({"type": "assistant", "uuid": "call-1", "parentUuid": "result-0",
                   "message": {"id": "msg_1", "content": [{"type": "tool_use", "id": "toolu_1"}]}}),
        ),
        obj(
            json!({"type": "user", "uuid": "result-1", "parentUuid": "call-1",
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1"}]}}),
        ),
    ];
    assert_eq!(
        chain_uuids(&entries),
        ["prompt", "call-0", "result-0", "call-1", "result-1"]
    );
}

#[test]
fn abandoned_branch_is_not_recovered() {
    let (calls, results) = parallel_turn("prompt", 2, &[0, 1]);
    let mut entries = vec![
        prompt(),
        obj(
            json!({"type": "assistant", "uuid": "old-call", "parentUuid": "prompt",
                   "message": {"id": "msg_abandoned", "content": [{"type": "tool_use", "id": "toolu_old"}]}}),
        ),
        obj(
            json!({"type": "user", "uuid": "old-result", "parentUuid": "old-call",
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_old"}]}}),
        ),
    ];
    entries.extend(calls);
    entries.extend(results);
    assert_eq!(
        chain_uuids(&entries),
        ["prompt", "call-0", "call-1", "result-0", "result-1"]
    );
}

#[test]
fn recovers_result_whose_parent_is_outside_the_turn() {
    let (calls, mut results) = parallel_turn("prompt", 2, &[0, 1]);
    results[0].insert("parentUuid".to_string(), json!("missing"));
    results[0].shift_remove("sourceToolAssistantUUID");
    let entries = turn(calls, results, vec![answer("result-1")]);
    assert_eq!(
        chain_uuids(&entries),
        ["prompt", "call-0", "call-1", "result-0", "result-1", "answer"]
    );
}

#[test]
fn does_not_duplicate_an_answered_call() {
    let (calls, results) = parallel_turn("prompt", 2, &[0, 1]);
    let mut duplicate = results[1].clone();
    duplicate.insert("uuid".to_string(), json!("result-1-copy"));
    duplicate.insert("parentUuid".to_string(), json!("elsewhere"));
    duplicate.shift_remove("sourceToolAssistantUUID");
    let entries = turn(calls, results, vec![duplicate, answer("result-1")]);
    assert_eq!(
        chain_uuids(&entries),
        ["prompt", "call-0", "call-1", "result-0", "result-1", "answer"]
    );
}

#[test]
fn does_not_pull_in_another_agents_result() {
    let (calls, mut results) = parallel_turn("prompt", 2, &[0, 1]);
    results[0].insert("parentUuid".to_string(), json!("sidechain-call"));
    results[0].insert("isSidechain".to_string(), json!(true));
    results[0].insert("agentId".to_string(), json!("agent-1"));
    results[0].shift_remove("sourceToolAssistantUUID");
    let entries = turn(calls, results, vec![answer("result-1")]);
    assert_eq!(
        chain_uuids(&entries),
        ["prompt", "call-0", "call-1", "result-1", "answer"]
    );
}

#[test]
fn recovery_tolerates_malformed_entries() {
    let (calls, mut results) = parallel_turn("prompt", 2, &[0, 1]);
    results[0].insert("timestamp".to_string(), json!(12345));
    let tail = vec![
        obj(json!({"type": "assistant", "uuid": "no-message", "parentUuid": "result-1"})),
        obj(
            json!({"type": "assistant", "uuid": "odd", "parentUuid": "no-message",
                   "message": {"id": 7, "content": ["text", null, {"type": "tool_use"}]}}),
        ),
        obj(
            json!({"type": "user", "uuid": "odd-result", "parentUuid": "odd",
                   "message": {"content": [{"type": "tool_result", "tool_use_id": 3}]}}),
        ),
    ];
    let entries = turn(calls, results, tail);
    assert_eq!(
        chain_uuids(&entries),
        [
            "prompt",
            "call-0",
            "call-1",
            "result-0",
            "result-1",
            "no-message",
            "odd",
            "odd-result"
        ]
    );
}
