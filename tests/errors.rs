use claude_code_sdk_rust::{
    CLIConnectionError, CLIJSONDecodeError, CLINotFoundError, ClaudeSDKError, ProcessError,
};

#[test]
fn cli_not_found_error_formats_message_and_path() {
    let error = CLINotFoundError::new("Claude Code not found", "/missing/claude");
    let sdk_error = ClaudeSDKError::from(error);

    let message = sdk_error.to_string();
    assert!(message.contains("Claude Code not found"));
    assert!(message.contains("/missing/claude"));
}

#[test]
fn connection_error_formats_message() {
    let sdk_error = ClaudeSDKError::from(CLIConnectionError::new("failed to connect"));

    assert!(sdk_error.to_string().contains("failed to connect"));
}

#[test]
fn process_error_preserves_exit_code_and_stderr() {
    let error = ProcessError::new("Process failed", Some(1), "Command not found");

    assert_eq!(error.exit_code, Some(1));
    assert_eq!(error.stderr, "Command not found");
    let message = error.to_string();
    assert!(message.contains("Process failed"));
    assert!(message.contains("exit code: 1"));
    assert!(message.contains("Command not found"));
}

#[test]
fn json_decode_error_preserves_short_line_and_source() {
    let original = serde_json::from_str::<serde_json::Value>("{invalid json}").unwrap_err();
    let error = CLIJSONDecodeError::new("{invalid json}", original);

    assert_eq!(error.line, "{invalid json}");
    assert!(error.to_string().contains("Failed to decode JSON"));
}

// ---------------------------------------------------------------------------
// Upstream v0.2.189 error parity
// ---------------------------------------------------------------------------

use claude_code_sdk_rust::{MessageParseError, ResultError};
use serde_json::json;

#[test]
fn wrapper_variants_display_the_bare_message() {
    assert_eq!(
        ClaudeSDKError::from(CLIConnectionError::new("failed to connect")).to_string(),
        "failed to connect"
    );
    assert_eq!(
        ClaudeSDKError::from(MessageParseError::new("bad payload")).to_string(),
        "bad payload"
    );
    assert_eq!(
        ClaudeSDKError::from(ProcessError::new("Command failed", Some(2), "")).to_string(),
        "Command failed (exit code: 2)"
    );
}

#[test]
fn cli_not_found_error_matches_python_format() {
    assert_eq!(
        CLINotFoundError::new("Claude Code not found", "/missing/claude").to_string(),
        "Claude Code not found: /missing/claude"
    );
    assert_eq!(
        CLINotFoundError::default().to_string(),
        "Claude Code not found"
    );
    assert_eq!(CLINotFoundError::default().cli_path, None);
    assert_eq!(CLINotFoundError::without_path("nope").to_string(), "nope");
    // CLINotFoundError is-a CLIConnectionError upstream.
    let error = ClaudeSDKError::from(CLINotFoundError::default());
    assert!(error.is_connection_error());
    assert!(ClaudeSDKError::from(CLIConnectionError::new("x")).is_connection_error());
    assert!(!error.is_process_error());
}

#[test]
fn json_decode_error_keeps_full_line_and_truncates_by_chars() {
    let line = format!("{{\"text\":\"{}\"}}", "é".repeat(150));
    let original = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let error = CLIJSONDecodeError::new(line.clone(), original);
    assert_eq!(error.line, line, "full line is preserved");
    let expected: String = line.chars().take(100).collect();
    assert_eq!(
        error.to_string(),
        format!("Failed to decode JSON: {expected}...")
    );

    let short = CLIJSONDecodeError::new("{x", "custom cause");
    assert_eq!(short.to_string(), "Failed to decode JSON: {x...");
    assert_eq!(short.original_error.to_string(), "custom cause");
}

#[test]
fn result_error_extracts_typed_attributes_from_payload() {
    let data = json!({
        "type": "result",
        "subtype": "success",
        "is_error": true,
        "errors": ["  first  ", "", 3, "second"],
        "result": "API Error: overloaded",
        "api_error_status": 529,
        "terminal_reason": "api_error",
        "session_id": "sess-1"
    });
    let error = ResultError::new(
        "Claude Code returned an error result: first; second",
        data.as_object().unwrap().clone(),
        Some(1),
    );
    assert_eq!(error.subtype.as_deref(), Some("success"));
    assert_eq!(
        error.errors,
        vec!["first".to_string(), "second".to_string()]
    );
    assert_eq!(error.result.as_deref(), Some("API Error: overloaded"));
    assert_eq!(error.api_error_status, Some(529));
    assert_eq!(error.terminal_reason.as_deref(), Some("api_error"));
    assert_eq!(error.session_id.as_deref(), Some("sess-1"));
    assert_eq!(error.exit_code, Some(1));
    assert_eq!(error.data["is_error"], true);
    assert_eq!(
        error.to_string(),
        "Claude Code returned an error result: first; second (exit code: 1)"
    );

    // ResultError is-a ProcessError upstream.
    let sdk_error = ClaudeSDKError::from(error);
    assert!(sdk_error.is_process_error());
    assert_eq!(sdk_error.exit_code(), Some(1));
    assert!(sdk_error.as_result_error().is_some());
}

#[test]
fn result_error_ignores_mistyped_attributes() {
    let data = json!({
        "subtype": 7,
        "errors": "bare string",
        "result": ["x"],
        "api_error_status": "529",
        "terminal_reason": null
    });
    let error = ResultError::new("m", data.as_object().unwrap().clone(), None);
    assert_eq!(error.subtype, None);
    assert_eq!(error.errors, vec!["bare string".to_string()]);
    assert_eq!(error.result, None);
    assert_eq!(error.api_error_status, None);
    assert_eq!(error.terminal_reason, None);
    assert_eq!(error.to_string(), "m");
}
