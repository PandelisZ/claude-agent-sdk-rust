# Upstream Parity Reference

Upstream repo: `https://github.com/anthropics/claude-agent-sdk-python`
Python import: `claude_agent_sdk`  |  Rust import: `claude_agent_sdk`
(crate published as `claude-code-sdk-rust`).

## The public surface = the parity contract

The authoritative list of what must exist in Rust is upstream
`src/claude_agent_sdk/__init__.py`'s `__all__` (~125 symbols). `types.py`
(~2100 lines) holds the field-level detail. `scripts/audit_parity.sh` extracts
`__all__` and diffs it against the Rust crate; trust its **NEW** bucket as the
port list and `parity_known.tsv` as the memory of everything already decided.

What the symbol audit does **not** catch (check the `git log`/diff for these):
- new fields on an existing type/message (same symbol, new shape),
- changed serde tag/wire string values,
- new CLI flags on `ClaudeAgentOptions` (mapped in `internal/cli_args.rs`),
- behavior/bugfixes inside `_internal/`.

## Rust naming/shape conventions (so audit hits are real)

- Python `Mcp*` -> Rust `MCP*` (plus `Mcp*` aliases in `lib.rs`). Casing only.
- Python message classes (`UserMessage`, `ResultMessage`, ...) -> variants of
  the Rust `Message` enum (`Message::UserMsg`, `Message::ResultMsg`, ...).
- Python content blocks (`TextBlock`, `ToolUseBlock`, `ServerToolUseBlock`, ...)
  -> variants of the Rust `ContentBlock` enum.
- Python `ThinkingConfig{Adaptive,Enabled,Disabled}` TypedDicts -> one Rust
  `ThinkingConfig` struct discriminated by `ThinkingConfigType`.
- Python `PermissionResultAllow/Deny` -> `PermissionResult` enum variants.
- Per-event hook input/output TypedDicts -> currently one generic
  `HookEventMessage` (see deferred rows in `parity_known.tsv`).
- `__version__` -> `VERSION`; `CanUseTool` -> `CanUseToolCallback`;
  `ToolAnnotations` -> `MCPToolAnnotations`; `SdkPluginConfig` -> `SDKPluginConfig`.

These are encoded as `mapped` rows in `parity_known.tsv` so the audit treats
them as covered. Add a row whenever you port a symbol to a non-matching name.

## Worked example: porting a new message type (`task_updated`, v0.2.106)

The reference port pattern, end to end:
1. Status/const types in `src/types.rs` (`TaskUpdatedStatus` enum,
   `TERMINAL_TASK_STATUSES`, `is_terminal_task_status`).
2. Message struct in `src/types/messages.rs` + a `Message::TaskUpdatedMsg`
   variant (use `#[serde(skip_serializing, skip_deserializing)]` for
   parser-synthesized system messages; import the type at the top).
3. Parser dispatch in `src/internal/parser.rs`: add a `Some("task_updated")`
   arm in `parse_system_message_value` and a `parse_task_updated` that derives
   fields from the raw payload exactly like the Python `message_parser.py`
   (e.g. `status` comes from `patch.status`, parsed defensively to `None`).
4. Tests in `tests/wire_parity.rs` (pin the JSON) + a presence assert in
   `tests/root_exports.rs`.
Cross-check the Rust parser against `_internal/message_parser.py` for the exact
field-extraction semantics, not just the type shape.

## Python module -> Rust module map

| Python (`src/claude_agent_sdk/`)        | Rust (`src/`)                          | Notes |
|-----------------------------------------|----------------------------------------|-------|
| `client.py` (`ClaudeSDKClient`)         | `client.rs` (`ClaudeAgentClient`)      | `ClaudeSDKClient` is a type alias in `lib.rs`. |
| `query.py` (`query`)                    | `query.rs`                             | One-shot + streaming query entry points. |
| `types.py`                              | `types.rs` + `types/`                  | Split: `messages.rs`, `hooks.rs`, `config.rs`, `agent_options.rs`. |
| options / `ClaudeAgentOptions`          | `options.rs` + `types/agent_options.rs`| Builder lives in `options.rs`. |
| `_internal/transport/`                  | `internal/transport.rs`, `internal/cli_discovery.rs` | CLI discovery + process transport. |
| `_internal/message_parser.py`           | `internal/message_parser.rs`, `internal/parser.rs` | Wire JSON -> typed `Message`. |
| control protocol / `_internal/query.py` | `internal/control.rs`, `internal/protocol.rs`, `internal/runtime.rs` | Control requests/responses. |
| CLI flag building                       | `internal/cli_args.rs`                 | Maps options -> `claude` CLI args. |
| in-process MCP (`create_sdk_mcp_server`)| `mcp.rs`, `internal/sdk_mcp.rs`        | SDK MCP servers + `tool` helpers. |
| sessions / session store                | `sessions.rs`, `sessions/`, `session_store.rs`, `session_summary.rs`, `internal/sessions_fs.rs`, `internal/session_resume.rs` | Local + store-backed session helpers. |
| errors                                  | `error.rs`                             | `ClaudeSDKError` and variants. |

When a Python file has no clear Rust counterpart, check `lib.rs` re-exports
first; the public surface is centralized there.

## Wire / serde conventions (do not break)

- JSON keys and enum tag values must match the Python SDK / CLI exactly. Most
  structs use `#[serde(rename_all = "camelCase")]`; some message tags are
  explicit (e.g. `PermissionUpdate.type = "addRules"`). Match upstream strings
  verbatim.
- Unknown message `type` values must be skipped for forward-compat, not error
  (see `wire_parity.rs::parser_skips_unknown_message_types_for_forward_compatibility`).
- New optional fields should be `Option<T>` with `#[serde(default, skip_serializing_if = ...)]`
  unless upstream always emits them.
- CLI args: every new `ClaudeAgentOptions` field that maps to a CLI flag needs a
  corresponding arm in `internal/cli_args.rs` and a test in
  `tests/options_validation.rs` or `internal/cli_args_tests.rs`.

## Parity tests (add/update these per change)

| Concern                              | Test file |
|--------------------------------------|-----------|
| Message wire shapes (CLI JSON)       | `tests/wire_parity.rs` |
| Option/type contracts vs Python      | `tests/type_contracts.rs` |
| CLI arg generation                   | `tests/options_validation.rs`, `src/internal/cli_args_tests.rs` |
| Control protocol                     | `tests/control_protocol.rs`, `tests/client_control.rs` |
| SDK MCP helpers                      | `tests/sdk_mcp_helpers.rs` |
| Sessions / store                     | `tests/session_*.rs`, `tests/local_sessions.rs` |
| Errors                               | `tests/errors.rs` |
| Root re-exports present              | `tests/root_exports.rs` |

Every wire/type parity change should be expressed as a test that pins the exact
Python JSON shape, then implemented until it passes.

## Common upstream change types -> where to land them

- New `ClaudeAgentOptions` field: `types/agent_options.rs` (struct) +
  `options.rs` (builder method) + `internal/cli_args.rs` (CLI mapping) + test.
- New message type/content block: `types/messages.rs` + `internal/parser.rs` +
  `tests/wire_parity.rs`.
- New permission/hook shape: `types/hooks.rs` or `config.rs` + `type_contracts.rs`.
- New control request: `internal/control.rs` / `protocol.rs` + `control_protocol.rs`.
- New public function: implement + re-export in `lib.rs` + `root_exports.rs`.
