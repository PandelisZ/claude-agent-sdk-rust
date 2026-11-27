use thiserror::Error;

type JsonMap = serde_json::Map<String, serde_json::Value>;

/// Base error type for SDK-specific failures.
///
/// `Display` of the wrapper variants is the wrapped error's own message
/// (Python's `str(e)` is the bare message, with no category prefix).
#[derive(Debug, Error)]
pub enum ClaudeSDKError {
    #[error(transparent)]
    CLIConnection(#[from] CLIConnectionError),

    /// The CLI could not be found. Upstream `CLINotFoundError` subclasses
    /// `CLIConnectionError`; see [`ClaudeSDKError::is_connection_error`].
    #[error(transparent)]
    CLINotFound(CLINotFoundError),

    #[error(transparent)]
    Process(#[from] ProcessError),

    /// The CLI exited after reporting a terminal error result. Upstream
    /// `ResultError` subclasses `ProcessError`; see
    /// [`ClaudeSDKError::is_process_error`].
    #[error(transparent)]
    Result(Box<ResultError>),

    #[error(transparent)]
    CLIJSONDecode(#[from] CLIJSONDecodeError),

    #[error(transparent)]
    MessageParse(#[from] MessageParseError),

    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Session error: {0}")]
    Session(String),

    #[error("MCP error: {0}")]
    MCP(String),

    #[error("Control request error: {subtype} - {message}")]
    ControlRequest { subtype: String, message: String },

    #[error("{0}")]
    Other(String),
}

impl From<ResultError> for ClaudeSDKError {
    fn from(err: ResultError) -> Self {
        ClaudeSDKError::Result(Box::new(err))
    }
}

impl From<CLINotFoundError> for ClaudeSDKError {
    fn from(err: CLINotFoundError) -> Self {
        ClaudeSDKError::CLINotFound(err)
    }
}

impl ClaudeSDKError {
    /// `true` for errors Python raises as `CLIConnectionError` (including its
    /// `CLINotFoundError` subclass).
    pub fn is_connection_error(&self) -> bool {
        matches!(
            self,
            ClaudeSDKError::CLIConnection(_) | ClaudeSDKError::CLINotFound(_)
        )
    }

    /// `true` for errors Python raises as `ProcessError` (including its
    /// `ResultError` subclass), so `except ProcessError` style handling
    /// catches both.
    pub fn is_process_error(&self) -> bool {
        matches!(self, ClaudeSDKError::Process(_) | ClaudeSDKError::Result(_))
    }

    /// The CLI exit code carried by a process or result error.
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            ClaudeSDKError::Process(err) => err.exit_code,
            ClaudeSDKError::Result(err) => err.exit_code,
            _ => None,
        }
    }

    /// The [`ResultError`] when this is one.
    pub fn as_result_error(&self) -> Option<&ResultError> {
        match self {
            ClaudeSDKError::Result(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

/// Reports failures when the SDK cannot connect to the CLI
#[derive(Debug, Error)]
#[error("{message}")]
pub struct CLIConnectionError {
    pub message: String,
}

impl CLIConnectionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Reports that the Claude CLI could not be located.
///
/// Formats as `"{message}: {cli_path}"` when a path is set, else `"{message}"`.
#[derive(Debug, Error)]
pub struct CLINotFoundError {
    pub message: String,
    pub cli_path: Option<String>,
}

impl std::fmt::Display for CLINotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.cli_path {
            Some(path) => write!(f, "{}: {}", self.message, path),
            None => f.write_str(&self.message),
        }
    }
}

impl Default for CLINotFoundError {
    fn default() -> Self {
        Self {
            message: "Claude Code not found".to_string(),
            cli_path: None,
        }
    }
}

impl CLINotFoundError {
    /// Build an error with a message and CLI path; an empty path is treated as
    /// absent (Python's falsy `cli_path`).
    pub fn new(message: impl Into<String>, cli_path: impl Into<String>) -> Self {
        let cli_path = cli_path.into();
        Self {
            message: message.into(),
            cli_path: (!cli_path.is_empty()).then_some(cli_path),
        }
    }

    /// Build an error with a message and no CLI path.
    pub fn without_path(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cli_path: None,
        }
    }
}

/// Reports CLI process failures
#[derive(Debug)]
pub struct ProcessError {
    pub message: String,
    pub exit_code: Option<i32>,
    pub stderr: String,
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(code) = self.exit_code {
            write!(f, " (exit code: {})", code)?;
        }
        if !self.stderr.is_empty() {
            write!(f, "\nError output: {}", self.stderr)?;
        }
        Ok(())
    }
}

impl std::error::Error for ProcessError {}

impl ProcessError {
    pub fn new(
        message: impl Into<String>,
        exit_code: Option<i32>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            exit_code,
            stderr: stderr.into(),
        }
    }
}

/// Normalize the `errors` field of a `result` frame to clean strings
/// (Python `_normalize_result_errors`): a bare string becomes `[s]`, a
/// non-list becomes `[]`, non-string and blank entries are dropped, the rest
/// are trimmed.
pub(crate) fn normalize_result_errors(raw: Option<&serde_json::Value>) -> Vec<String> {
    match raw {
        Some(serde_json::Value::String(error)) => {
            let trimmed = error.trim();
            if trimmed.is_empty() {
                Vec::new()
            } else {
                vec![trimmed.to_string()]
            }
        }
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Pick the most informative text from a `result` frame with `is_error`
/// (Python `_internal/query.py::_error_result_text`): normalized `errors`
/// joined with `"; "`, then the trimmed `result`, then a non-`"success"`
/// `subtype`, then `"API error (HTTP {status})"`, else `"unknown error"`.
pub(crate) fn error_result_text(data: &JsonMap) -> String {
    let errors = normalize_result_errors(data.get("errors"));
    if !errors.is_empty() {
        return errors.join("; ");
    }
    if let Some(result) = data.get("result").and_then(|value| value.as_str()) {
        let result = result.trim();
        if !result.is_empty() {
            return result.to_string();
        }
    }
    if let Some(subtype) = data.get("subtype").and_then(|value| value.as_str()) {
        if !subtype.is_empty() && subtype != "success" {
            return subtype.to_string();
        }
    }
    match data.get("api_error_status") {
        None | Some(serde_json::Value::Null) => "unknown error".to_string(),
        Some(serde_json::Value::String(status)) => format!("API error (HTTP {status})"),
        Some(status) => format!("API error (HTTP {status})"),
    }
}

/// Raised when the CLI exits after reporting a terminal error result.
///
/// The CLI ends a failed run by emitting a `result` message with
/// `is_error: true` and then exiting non-zero. This replaces the bare
/// "exit code 1" [`ProcessError`] for that case and carries the result's
/// payload, so callers can branch on *why* the run failed (e.g.
/// `terminal_reason == Some("api_error")`, `subtype == Some("error_max_turns")`).
///
/// Upstream it subclasses `ProcessError`: use
/// [`ClaudeSDKError::is_process_error`] / [`ClaudeSDKError::exit_code`] to
/// handle both. `Display` is `"{message} (exit code: N)"`; stderr is
/// deliberately not carried over.
#[derive(Debug, Clone)]
pub struct ResultError {
    pub message: String,
    /// The raw `result` message payload as emitted by the CLI.
    pub data: JsonMap,
    /// The result subtype (`"error_max_turns"`, `"error_during_execution"`,
    /// ... or `"success"` when the last turn was an API error).
    pub subtype: Option<String>,
    /// Error strings reported by the CLI (normalized; may be empty).
    pub errors: Vec<String>,
    /// The result text, if any (for API failures the `"API Error: ..."` prose).
    pub result: Option<String>,
    /// HTTP status of the failing API call, if any.
    pub api_error_status: Option<i64>,
    /// Why the run ended (e.g. `"api_error"`, `"max_turns"`), if reported.
    pub terminal_reason: Option<String>,
    /// Session the result belongs to, if reported.
    pub session_id: Option<String>,
    /// Exit code of the CLI process.
    pub exit_code: Option<i32>,
}

impl std::fmt::Display for ResultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(code) = self.exit_code {
            write!(f, " (exit code: {code})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ResultError {}

impl ResultError {
    /// Build a result error from a message and a raw `result` payload, reading
    /// each typed attribute only when it has the expected JSON type.
    pub fn new(message: impl Into<String>, data: JsonMap, exit_code: Option<i32>) -> Self {
        let string = |key: &str| {
            data.get(key)
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        };
        Self {
            message: message.into(),
            subtype: string("subtype"),
            errors: normalize_result_errors(data.get("errors")),
            result: string("result"),
            api_error_status: data
                .get("api_error_status")
                .and_then(|value| value.as_i64()),
            terminal_reason: string("terminal_reason"),
            session_id: string("session_id"),
            exit_code,
            data,
        }
    }

    /// Build the error raised in place of the trailing [`ProcessError`] after
    /// an `is_error` result frame (message
    /// `"Claude Code returned an error result: {text}"`).
    pub(crate) fn from_error_result(data: &JsonMap, exit_code: Option<i32>) -> Self {
        Self::new(
            format!(
                "Claude Code returned an error result: {}",
                error_result_text(data)
            ),
            data.clone(),
            exit_code,
        )
    }
}

/// Tracks the most recent `result` frame with `is_error: true` so the
/// trailing [`ProcessError`] can be replaced by a [`ResultError`] (Python
/// `Query._last_error_result`).
///
/// Feed every non-control message to [`ErrorResultTracker::observe`]; pass
/// the transport's terminal error through [`ErrorResultTracker::replace`].
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub(crate) struct ErrorResultTracker {
    last_error_result: Option<JsonMap>,
}

#[allow(dead_code)]
impl ErrorResultTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record a raw stdout message. A result sets (is_error) or clears the
    /// remembered payload; any other message except `system` /
    /// `session_state_changed` clears it. Control frames and
    /// `transcript_mirror` frames are ignored (upstream routes them before
    /// this bookkeeping).
    pub(crate) fn observe(&mut self, message: &serde_json::Value) {
        let msg_type = message.get("type").and_then(|value| value.as_str());
        match msg_type {
            Some(
                "control_response"
                | "control_request"
                | "control_cancel_request"
                | "transcript_mirror",
            ) => {}
            Some("result") => {
                let is_error = message
                    .get("is_error")
                    .is_some_and(|value| !crate::types::messages::is_falsy(value));
                self.last_error_result = if is_error {
                    message.as_object().cloned()
                } else {
                    None
                };
            }
            Some("system")
                if message.get("subtype").and_then(|value| value.as_str())
                    == Some("session_state_changed") => {}
            _ => self.last_error_result = None,
        }
    }

    /// The remembered error result payload, if any.
    pub(crate) fn last_error_result(&self) -> Option<&JsonMap> {
        self.last_error_result.as_ref()
    }

    /// Replace a [`ClaudeSDKError::Process`] with a [`ClaudeSDKError::Result`]
    /// when an error result is remembered; other errors pass through.
    pub(crate) fn replace(&self, error: ClaudeSDKError) -> ClaudeSDKError {
        match (error, &self.last_error_result) {
            (ClaudeSDKError::Process(process), Some(data)) => {
                ClaudeSDKError::from(ResultError::from_error_result(data, process.exit_code))
            }
            (error, _) => error,
        }
    }
}

/// Reports malformed JSON from CLI stdout.
///
/// `line` is the full offending line; `Display` is
/// `"Failed to decode JSON: {first 100 chars}..."`.
#[derive(Debug, Error)]
pub struct CLIJSONDecodeError {
    pub line: String,
    #[source]
    pub original_error: Box<dyn std::error::Error + Send + Sync>,
}

impl std::fmt::Display for CLIJSONDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let preview: String = self.line.chars().take(100).collect();
        write!(f, "Failed to decode JSON: {preview}...")
    }
}

impl CLIJSONDecodeError {
    pub fn new(
        line: impl Into<String>,
        original_error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            line: line.into(),
            original_error: original_error.into(),
        }
    }
}

/// Reports malformed, known CLI payloads
#[derive(Debug, Error)]
#[error("{message}")]
pub struct MessageParseError {
    pub message: String,
    pub data: Option<serde_json::Map<String, serde_json::Value>>,
}

impl MessageParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Map<String, serde_json::Value>) -> Self {
        self.data = Some(data);
        self
    }
}

/// Map internal transport errors to public SDK errors
pub fn map_transport_error<E>(err: E) -> ClaudeSDKError
where
    E: std::error::Error + 'static,
{
    // Try downcasting to specific error types
    if let Some(e) =
        (&err as &(dyn std::error::Error + 'static)).downcast_ref::<CLIConnectionError>()
    {
        return ClaudeSDKError::CLIConnection(CLIConnectionError::new(&e.message));
    }

    ClaudeSDKError::Other(err.to_string())
}

/// Result type alias for SDK operations
pub type Result<T> = std::result::Result<T, ClaudeSDKError>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: serde_json::Value) -> JsonMap {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn error_result_text_prefers_errors_then_result_then_subtype_then_status() {
        assert_eq!(
            error_result_text(&map(json!({"errors": [" a ", "", "b"], "result": "r"}))),
            "a; b"
        );
        assert_eq!(
            error_result_text(&map(
                json!({"errors": [], "result": "  API Error: 529  ", "subtype": "success"})
            )),
            "API Error: 529"
        );
        assert_eq!(
            error_result_text(&map(json!({"result": "   ", "subtype": "error_max_turns"}))),
            "error_max_turns"
        );
        assert_eq!(
            error_result_text(&map(json!({"subtype": "success", "api_error_status": 500}))),
            "API error (HTTP 500)"
        );
        assert_eq!(
            error_result_text(&map(json!({"subtype": "success"}))),
            "unknown error"
        );
        assert_eq!(error_result_text(&map(json!({"errors": "bare"}))), "bare");
    }

    #[test]
    fn tracker_replaces_trailing_process_error_after_error_result() {
        let mut tracker = ErrorResultTracker::new();
        tracker.observe(&json!({
            "type": "result", "subtype": "error_max_turns", "is_error": true,
            "errors": ["Reached max turns"], "session_id": "s"
        }));
        // Control frames and the post-turn session state marker keep it.
        tracker.observe(&json!({"type": "control_request", "request_id": "1"}));
        tracker.observe(
            &json!({"type": "system", "subtype": "session_state_changed", "state": "idle"}),
        );
        let replaced = tracker.replace(ClaudeSDKError::Process(ProcessError::new(
            "Claude Code process exited with error",
            Some(1),
            "stderr text",
        )));
        match replaced {
            ClaudeSDKError::Result(err) => {
                assert_eq!(
                    err.to_string(),
                    "Claude Code returned an error result: Reached max turns (exit code: 1)"
                );
                assert_eq!(err.subtype.as_deref(), Some("error_max_turns"));
                assert_eq!(err.session_id.as_deref(), Some("s"));
            }
            other => panic!("expected ResultError, got {other:?}"),
        }
        // Non-process errors pass through untouched.
        assert!(matches!(
            tracker.replace(ClaudeSDKError::Other("x".into())),
            ClaudeSDKError::Other(_)
        ));
    }

    #[test]
    fn tracker_clears_on_success_result_and_other_messages() {
        let mut tracker = ErrorResultTracker::new();
        tracker.observe(&json!({"type": "result", "is_error": true}));
        tracker.observe(&json!({"type": "result", "is_error": false}));
        assert!(tracker.last_error_result().is_none());

        tracker.observe(&json!({"type": "result", "is_error": true}));
        tracker.observe(&json!({"type": "assistant", "message": {}}));
        assert!(tracker.last_error_result().is_none());
        assert!(matches!(
            tracker.replace(ClaudeSDKError::Process(ProcessError::new("p", Some(1), ""))),
            ClaudeSDKError::Process(_)
        ));
    }
}
