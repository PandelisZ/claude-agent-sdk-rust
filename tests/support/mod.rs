//! A scripted in-memory [`Transport`] for control-protocol tests.
//!
//! `read()` answers `initialize` once its request is written, then returns
//! scripted frames; when none is queued it waits (like a live CLI) until a
//! frame is pushed, the transport is closed, or `eof_when_drained` is set.
//! Every step is cancel-safe, as the SDK's background reader requires.
#![allow(dead_code)]

use async_trait::async_trait;
use claude_code_sdk_rust::{ClaudeSDKError, ProcessError, Result, Transport};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Notify;

pub enum ReadItem {
    Line(Value),
    /// `ProcessError("Command failed with exit code N")`.
    ProcessExit(i32),
    Eof,
}

pub struct MockState {
    pub connected: usize,
    pub writes: Vec<Value>,
    pub reads: VecDeque<ReadItem>,
    /// The initialize response body; `None` leaves initialize unanswered.
    pub init_response: Option<Value>,
    answered_init: Option<String>,
    /// Answer every non-initialize control request with `{}`.
    pub auto_control_responses: bool,
    auto_cursor: usize,
    pub eof_when_drained: bool,
    pub input_closed: bool,
    pub closed: bool,
    /// `write:<type>`, `read:<type>`, `close_input`, `close`, in order.
    pub log: Vec<String>,
    pub connect_error: Option<String>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            connected: 0,
            writes: Vec::new(),
            reads: VecDeque::new(),
            init_response: Some(serde_json::json!({"commands": []})),
            answered_init: None,
            auto_control_responses: false,
            auto_cursor: 0,
            eof_when_drained: false,
            input_closed: false,
            closed: false,
            log: Vec::new(),
            connect_error: None,
        }
    }
}

impl MockState {
    /// Written frames of `type`.
    pub fn writes_of(&self, frame_type: &str) -> Vec<Value> {
        self.writes
            .iter()
            .filter(|write| write["type"] == frame_type)
            .cloned()
            .collect()
    }

    /// The control response written for `request_id`.
    pub fn response_for(&self, request_id: &str) -> Option<Value> {
        self.writes
            .iter()
            .find(|write| {
                write["type"] == "control_response" && write["response"]["request_id"] == request_id
            })
            .cloned()
    }

    pub fn position(&self, entry: &str) -> Option<usize> {
        self.log.iter().position(|logged| logged == entry)
    }
}

#[derive(Clone, Default)]
pub struct MockTransport {
    pub state: Arc<Mutex<MockState>>,
    notify: Arc<Notify>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lock(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap()
    }

    pub fn push(&self, value: Value) {
        self.lock().reads.push_back(ReadItem::Line(value));
        self.notify.notify_one();
    }

    pub fn push_item(&self, item: ReadItem) {
        self.lock().reads.push_back(item);
        self.notify.notify_one();
    }

    pub fn boxed(&self) -> Box<dyn Transport> {
        Box::new(self.clone())
    }

    /// Wait (up to 5s) until `predicate` holds.
    pub async fn wait_until(&self, what: &str, predicate: impl Fn(&MockState) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if predicate(&self.lock()) {
                return;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("timed out waiting for {what}; log: {:?}", self.lock().log);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

fn control_success(request_id: &str, response: Value) -> Value {
    serde_json::json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response}
    })
}

#[async_trait]
impl Transport for MockTransport {
    async fn connect(&mut self) -> Result<()> {
        let mut state = self.lock();
        state.connected += 1;
        if let Some(error) = state.connect_error.clone() {
            return Err(claude_code_sdk_rust::CLIConnectionError::new(error).into());
        }
        Ok(())
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        {
            let mut state = self.lock();
            for line in data.split(|byte| *byte == b'\n') {
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let value: Value = serde_json::from_slice(line)?;
                let kind = value["type"].as_str().unwrap_or("?").to_string();
                state.log.push(format!("write:{kind}"));
                state.writes.push(value);
            }
        }
        self.notify.notify_one();
        Ok(())
    }

    async fn close_input(&mut self) -> Result<()> {
        {
            let mut state = self.lock();
            if !state.input_closed {
                state.log.push("close_input".to_string());
            }
            state.input_closed = true;
        }
        self.notify.notify_one();
        Ok(())
    }

    async fn read(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            let notified = self.notify.notified();
            {
                let mut state = self.lock();
                // Answer the latest initialize request once.
                let init_id = state
                    .writes
                    .iter()
                    .rev()
                    .find(|write| write["request"]["subtype"] == "initialize")
                    .and_then(|write| write["request_id"].as_str().map(String::from));
                if let (Some(id), Some(response)) = (init_id, state.init_response.clone()) {
                    if state.answered_init.as_deref() != Some(id.as_str()) {
                        state.answered_init = Some(id.clone());
                        return Ok(Some(serde_json::to_vec(&control_success(&id, response))?));
                    }
                }
                if state.auto_control_responses {
                    while state.auto_cursor < state.writes.len() {
                        let write = state.writes[state.auto_cursor].clone();
                        state.auto_cursor += 1;
                        if write["type"] != "control_request"
                            || write["request"]["subtype"] == "initialize"
                        {
                            continue;
                        }
                        let id = write["request_id"].as_str().unwrap_or_default();
                        return Ok(Some(serde_json::to_vec(&control_success(
                            id,
                            serde_json::json!({}),
                        ))?));
                    }
                }
                match state.reads.pop_front() {
                    Some(ReadItem::Line(value)) => {
                        let kind = value["type"].as_str().unwrap_or("?").to_string();
                        state.log.push(format!("read:{kind}"));
                        return Ok(Some(serde_json::to_vec(&value)?));
                    }
                    Some(ReadItem::ProcessExit(code)) => {
                        state.log.push("read:exit".to_string());
                        return Err(ClaudeSDKError::Process(ProcessError::new(
                            format!("Command failed with exit code {code}"),
                            Some(code),
                            "Check stderr output for details",
                        )));
                    }
                    Some(ReadItem::Eof) => return Ok(None),
                    None => {}
                }
                if state.closed || state.eof_when_drained {
                    return Ok(None);
                }
            }
            notified.await;
        }
    }

    async fn close(&mut self) -> Result<()> {
        {
            let mut state = self.lock();
            state.log.push("close".to_string());
            state.closed = true;
        }
        self.notify.notify_one();
        Ok(())
    }
}

pub fn assistant(text: &str) -> Value {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": text}]
        },
        "session_id": "default"
    })
}

pub fn result_frame() -> Value {
    serde_json::json!({
        "type": "result",
        "subtype": "success",
        "duration_ms": 1,
        "duration_api_ms": 1,
        "is_error": false,
        "num_turns": 1,
        "session_id": "default",
        "stop_reason": "end_turn"
    })
}

pub fn error_result_frame() -> Value {
    serde_json::json!({
        "type": "result",
        "subtype": "error_max_turns",
        "duration_ms": 1,
        "duration_api_ms": 1,
        "is_error": true,
        "num_turns": 3,
        "session_id": "sess-err",
        "errors": ["Reached maximum number of turns (3)"],
        "terminal_reason": "max_turns"
    })
}

pub fn session_state(state: &str, host_only: bool) -> Value {
    let mut frame = serde_json::json!({
        "type": "system",
        "subtype": "session_state_changed",
        "state": state,
        "uuid": "u",
        "session_id": "default"
    });
    if host_only {
        frame["sdk_host_only"] = Value::Bool(true);
    }
    frame
}
