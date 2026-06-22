# Upstream Sync Baseline

State file for the `sync-upstream-python-sdk` skill. Records the last upstream
commit of `anthropics/claude-agent-sdk-python` that this Rust crate was synced
to. Update both fields after each successful sync pass.

version: v0.2.106
commit: a56450b8303a3bde2b102a735f725d25b49081ee

## Sync log

<!-- Newest first. One line per sync: date - upstream version (commit) - notes. -->
- 2026-06-22 - v0.2.106 (a56450b) - Initial parity audit. Ported `task_updated`
  lifecycle message: `TaskUpdatedMessage`, `TaskUpdatedStatus`,
  `TERMINAL_TASK_STATUSES`, `is_terminal_task_status`, parser dispatch + wire
  tests. Deferred candidate gaps (represented generically today, not yet typed):
  `EffortLevel` int form; typed `ServerToolName` discriminator and generic
  `ServerToolResultBlock` tag; typed `ThinkingDisplay`; per-event typed hook
  inputs (`PreToolUseHookInput`, etc.); `McpSdkServerConfig` alias.
