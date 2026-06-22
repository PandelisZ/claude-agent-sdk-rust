# Upstream Parity Reference

Upstream repo: `https://github.com/anthropics/claude-agent-sdk-python`
Python import: `claude_agent_sdk`  |  Rust import: `claude_agent_sdk`
(crate published as `claude-code-sdk-rust`).

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
