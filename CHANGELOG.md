# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
(pre-1.0: breaking changes bump the minor version).

## [0.5.1] - 2026-11-26

### Fixed

- Restored the supervised spawned-stream API from 0.4.1, which 0.5.0
  dropped: `ClaudeAgentClient::spawn_stream_message_supervised`,
  `ClaudeAgentClient::run_client_stream`, `SpawnedStream` and
  `SpawnedStreamCleanup` (`wait_for_cleanup`). It is rebuilt on the 0.5.0
  runtime. `spawn_stream_message` now delegates to it.
- A spawned stream now stops as soon as its receiver is dropped, even while
  the CLI is idle. Before, it noticed only when the next message arrived. It
  always disconnects, which ends and reaps the CLI child.
- An abandoned spawned stream (receiver dropped mid-turn) now ends the CLI
  at once: SIGTERM, then SIGKILL, without the 5 s stdin-EOF grace period of a
  graceful close. A turn that completes still closes gracefully.

### Added

- `Transport::abort`, with a default that calls `close`, so existing custom
  transports are unaffected. The subprocess transport overrides it to signal
  the CLI without waiting for it to exit on stdin EOF.
- `SdkMcpTool::validate_input(false)` skips the input-schema check before
  the handler runs. Use it for handlers that accept or repair arguments
  outside their advertised schema (the MCP Python server's
  `validate_input=False`). Validation stays on by default.

## [0.5.0] - 2026-11-26

Full upstream parity pass against the Python `claude-agent-sdk` **v0.2.189**
(`anthropics/claude-agent-sdk-python`, commit `6c1b03f`), up from v0.2.106.
Covers the whole public surface (the parity audit reports 0 missing
symbols) plus wire and behavior compatibility: CLI argv and env, the
initialize and control protocol, message parsing, sessions, and SDK MCP.
Verified against Claude Code CLI 2.1.332.

### Added

- **Runtime:** a background reader/router (`internal/runtime.rs`).
  - Control requests (`can_use_tool`, `hook_callback`, `mcp_message`) are
    answered concurrently, each in its own task.
  - Control responses no longer drop interleaved messages.
  - `control_cancel_request` aborts in-flight handlers.
  - stdin is written from a dedicated task, so a large prompt can't deadlock
    against a full stdout pipe.
- **Streaming receive:** `receive_messages()` / `receive_response()` return a
  `MessageStream`. New `collect_messages()` / `collect_response()` helpers, and
  `stream_query` / `QueryPrompt` for one-shot streaming queries.
- **stdin run-end semantics** match Python: stdin stays open while hooks,
  `can_use_tool`, SDK MCP servers or deferring background tasks need it, until
  the session reports `idle`.
- **Errors:** `ResultError` (`ClaudeSDKError::Result`) carries the real error
  result text and payload. It replaces the trailing `ProcessError` after an
  `is_error` result.
- **Messages:**
  - `ConversationResetMessage` (`conversation_reset`).
  - `MessageOrigin` / `MessageOriginKind` / `TaskNotificationOriginSubkind`
    on user and result messages.
  - `ResultMessage.terminal_reason`.
  - `user_message_uuid(s)` on assistant, result and stream-event messages.
  - Typed `ModelUsage`.
  - `SystemInitData` and related types (`SystemInitMcpServer`/`Plugin`/
    `PluginError`, `FastModeState`, `FastModeDisabledReason`).
  - Unknown content blocks are skipped instead of failing the message.
- **Hooks:**
  - All 21 typed `*HookSpecificOutput` structs, `HookJSONOutput`
    (Sync/Async), `HookEvent`, `HookEffort`, `BackgroundTaskSummary` and
    `SessionCronSummary`.
  - New hook input fields.
  - `HookMatcher::on_failure` (fail-closed hooks are validated on initialize).
- **Permissions:**
  - Typed `PermissionBehavior` / `PermissionUpdateDestination` /
    `PermissionUpdateType`.
  - Permission updates are sent in the shape the CLI accepts.
  - `CanUseToolShadowedWarning` (logged when `allowed_tools` or
    `bypassPermissions` shadow `can_use_tool`).
  - `can_use_tool` now works with string prompts.
- **Options:**
  - New: `client_app` (with a derived default app label), `verbatim_prompts`,
    `forward_subagent_text`, `resume_session_at`, `resume_drops_turn`,
    `SystemPromptCustom`, and `SystemPromptPreset.snapshot` (sent as
    `systemPromptSnapshot`).
  - `ThinkingDisplay` and `AgentEffort` are typed.
  - Sandbox `ignoreViolations` is now a command-pattern map.
  - Skill names are validated.
- **SDK MCP:**
  - Async tool handlers (`async_tool`, `SdkMcpToolResult`) and custom
    `SdkMcpServer` implementations (`sdk_mcp_custom_server`).
  - Arguments are validated against the input schema.
  - JSON-RPC handling and content conversion match Python (ping,
    notifications, protocol negotiation).
- **Sessions:**
  - Python-shaped `SDKSessionInfo` / `SessionMessage`.
  - `get_session_messages` returns every parallel tool call and result, and
    follows the conversation chain.
  - Subagent `parent_tool_use_id` is recovered.
  - The user's `settings.json` is seeded into the temp config dir on
    SessionStore resume.
  - Store capability probes (`supports_list_session_summaries`,
    `supports_list_subkeys`).
- `Transport`, `SubprocessCLITransport` and `TransportOptions` are exported at
  the crate root.
- The README examples are compiled as doctests.

### Changed (breaking)

- **Client API**
  - `receive_messages`/`receive_response` return streams, not `Vec`s.
  - `stream_message` returns as soon as the prompt is written.
  - Client methods take `&self`.
  - `get_server_info` returns `Result<Option<&Map>>`.
  - `connect_with_stream` requires `'static + Send` streams.
  - Client prompts use session id `"default"` and include
    `parent_tool_use_id: null`.
- **Queries**
  - One-shot queries read until stdout EOF, not just to the first result.
  - Streamed messages no longer get a `session_id` injected.
- **Messages**
  - Status and error enums are open (`Other(String)`).
  - New fields on several `Message` variants, and a new
    `Message::ConversationResetMsg` variant.
  - Result durations and turn counts are `i64`.
  - Task message keys are serialized in snake_case.
- **Errors**
  - `Display` shows the bare upstream wording with no category prefix.
  - `CLINotFoundError.cli_path` is `Option<String>`.
  - `CLIJSONDecodeError` keeps the full line.
  - `ProcessError` text is `Command failed with exit code N`.
- **Options**
  - `extra_args` is an ordered `Vec<(String, Option<String>)>`.
  - The system prompt setters are mutually exclusive (last one wins).
  - argv order and formatting follow Python (e.g. `--resume=<id>` and
    `--session-id=<id>` as single tokens).
  - The child env is rebuilt (`CLAUDECODE` filtered, OTEL scrubbed,
    `CLAUDE_CODE_SDK_READS_SESSION_STATE=1`).
  - stderr is piped only when a callback is set.
  - `options.user` is applied.
  - Settings that can't be loaded alongside a sandbox fail closed.
  - The `claude -v` pre-spawn check is replaced by a check on the version in
    the init message.
- **Process shutdown:** stdin EOF, then wait 5s, then SIGTERM, then SIGKILL.
- **Hooks and permissions**
  - `HookMatcher` and several hook input structs have new fields.
  - `PermissionUpdate` fields are typed enums.
- **SDK MCP**
  - Tool annotation hints are `Option<bool>`.
  - `SimpleMCPServer::call_tool` is async and returns `SdkMcpToolResult`.
  - The MCP content key is `mimeType`.
- **Sessions**
  - `list_sessions`/`get_session_info`/`get_session_messages` return the
    Python shapes; invalid ids give empty results instead of errors.
  - `rename_session` writes `custom-title`.
  - Tags are NFKC-sanitized.
  - `delete_session` errors when the session is missing.
  - Session ids must be hyphenated UUIDs.
- **Types**
  - `ThinkingConfig.display` is `Option<ThinkingDisplay>`.
  - `AgentDefinition.effort` is typed.
  - `SandboxIgnoreViolations` is a map.
- **Serialization:** `serde_json` `preserve_order` is enabled, so map key order
  matches Python dict order.

### Fixed

- Stdout decoding: multi-byte UTF-8 split across reads, NDJSON framing, and a
  truncated tail at EOF.
- A first-prompt summary truncation panic on multi-byte text.
- Local `get_subagent_messages` dropping real subagent messages.
- A flaky ETXTBSY in tests; spawn now retries briefly on "Text file busy".

## [0.4.0] - 2026-06-22

### Added

- Image (multimodal) prompt input. New public types in `types::messages`
  (re-exported at the crate root):
  - `UserMessageInput` — either `Text(String)` or `Blocks(Vec<InputContentBlock>)`.
  - `InputContentBlock` — `Text { text }` or `Image { source }`.
  - `ImageSource` — `Base64 { media_type, data }` or `Url { url }`, with
    `ImageSource::from_data_url` to parse `data:<mime>;base64,<payload>` URLs.
- A user message's `content` is now serialized as a JSON string for text-only
  prompts, or as a content-block array (text + images) for multimodal prompts.

### Changed

- **Breaking:** the streaming/query entrypoints (`spawn_stream_message`,
  `stream_message`, `query`, `query_with_session_id`, `connect_with_prompt`,
  `send_message`) now accept `impl Into<UserMessageInput>` instead of
  `impl Into<String>`. `String` and `&str` callers are unaffected; callers
  passing other `Into<String>` types must convert to `String`/`UserMessageInput`.

## [0.3.1] - 2026-06-22

### Added

- End-to-end test coverage (all `#[ignore]`d; require an authenticated `claude`
  CLI and incur API usage):
  - `tests/e2e_tool_use.rs` — in-process SDK MCP tool dispatch, built-in Bash
    tool execution, `get_mcp_status` control round-trip, and interactive
    multi-turn context retention.
  - `tests/e2e_web_tools.rs` — built-in `WebSearch` and `WebFetch` tool
    invocation with live result round-tripping.

### Notes

- No library/runtime changes; this release only adds opt-in e2e tests, so it is
  API-compatible with `0.3.0`.

## [0.3.0] - 2026-06-22

Upstream parity pass against the Python `claude-agent-sdk` **v0.2.106**
(`anthropics/claude-agent-sdk-python`).

### Added

- `EffortLevel` enum (`low` / `medium` / `high` / `xhigh` / `max`) mirroring the
  upstream `EffortLevel` literal, serialized to the bare CLI string for
  `--effort`.
- `ServerToolName` enum for server-side tool names (`advisor`, `web_search`,
  `web_fetch`, `code_execution`, `bash_code_execution`,
  `text_editor_code_execution`, `tool_search_tool_regex`,
  `tool_search_tool_bm25`). Deserializes forward-compatibly: unknown names are
  preserved in `ServerToolName::Other` instead of failing to parse.
- Typed hook input/output payloads matching the Python type surface:
  `HookInput` (discriminated on `hook_event_name`), `BaseHookInput`, and the
  per-event `PreToolUseHookInput`, `PostToolUseHookInput`,
  `PostToolUseFailureHookInput`, `UserPromptSubmitHookInput`, `StopHookInput`,
  `SubagentStopHookInput`, `PreCompactHookInput`, `NotificationHookInput`,
  `SubagentStartHookInput`, `PermissionRequestHookInput`; plus hook-specific
  outputs `PostToolUseFailureHookSpecificOutput`,
  `NotificationHookSpecificOutput`, `SubagentStartHookSpecificOutput`, and
  `PermissionRequestHookSpecificOutput`. The hook callback itself stays generic
  over `serde_json::Value`.
- `task_updated` lifecycle system message: `TaskUpdatedMessage`,
  `TaskUpdatedStatus`, `TERMINAL_TASK_STATUSES`, and `is_terminal_task_status`.
  The parser derives `status` from `patch.status` and parses defensively.
- `ClaudeAgentClient::spawn_stream_message` convenience for fire-and-forget
  single-prompt streaming.

### Changed

- **Breaking:** `ClaudeAgentOptions::effort` is now `Option<EffortLevel>` (was
  `Option<String>`); the builder `effort(..)` takes an `EffortLevel`.
- **Breaking:** `ContentBlock::ServerToolUse::name` is now `ServerToolName` (was
  `String`).

## [0.2.0] - 2026-06

- Published to crates.io as `claude-code-sdk-rust` (library import path
  `claude_agent_sdk`).

## [0.1.1] - 2026-06

- Maintenance release.

## [0.1.0] - 2026-06

- Initial crates.io release: async Rust SDK for the Claude Code CLI with typed
  options, message parsing, interactive sessions, control requests, SDK MCP
  servers, and local/session-store helpers.

[0.5.0]: https://github.com/PandelisZ/claude-agent-sdk-rust/releases/tag/v0.5.0
[0.4.0]: https://github.com/PandelisZ/claude-agent-sdk-rust/releases/tag/v0.4.0
[0.3.0]: https://github.com/PandelisZ/claude-agent-sdk-rust/releases/tag/v0.3.0
[0.2.0]: https://crates.io/crates/claude-code-sdk-rust/0.2.0
[0.1.1]: https://crates.io/crates/claude-code-sdk-rust/0.1.1
[0.1.0]: https://crates.io/crates/claude-code-sdk-rust/0.1.0
