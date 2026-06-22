---
name: sync-upstream-python-sdk
description: >-
  Periodically update this Rust crate (claude-code-sdk-rust) to match the
  upstream Python reference implementation, anthropics/claude-agent-sdk-python.
  Use when asked to sync, update parity with, catch up to, or port new features
  from the Python Claude Agent SDK, or to run a periodic upstream-parity pass.
---

# Sync Rust SDK with Upstream Python SDK

This crate targets behavioral parity with the Python `claude-agent-sdk`
(repo: `anthropics/claude-agent-sdk-python`). This skill runs a repeatable
"catch up to upstream" pass: fetch upstream, diff against our last-synced
baseline, port new/changed features into Rust, and update the baseline.

## Workflow

Run these steps in order. Do not skip the baseline update at the end.

### 1. Fetch upstream and find what changed

```bash
.agents/skills/sync-upstream-python-sdk/scripts/fetch_upstream.sh
```

This clones/updates a cached checkout of the Python SDK and prints:
- the current upstream tag/commit,
- the baseline we last synced (from `UPSTREAM_BASELINE.md`),
- a `git log` and changed-file list between baseline and current.

If `fetch_upstream.sh` reports "already at baseline", there is nothing to do.

### 2. Triage upstream changes

For each changed area in the upstream diff, classify it:
- **Public API** (new option, type, message field, function) -> must port.
- **Wire/protocol** (CLI args, control protocol, JSON shapes) -> must port; these
  break parity silently if missed.
- **Behavior/bugfix** -> port if it affects observable behavior.
- **Python-only** (packaging, typing stubs, asyncio plumbing, docs) -> skip, but
  note it in the summary.

Use `reference.md` for the Python-module -> Rust-module map so you edit the
right files.

### 3. Port changes into Rust

- Follow existing Rust patterns in the target module (builder methods in
  `src/options.rs`, types in `src/types/`, wire parsing in `src/internal/`).
- Preserve the crate's naming convention: serde wire shapes must match the
  Python/CLI JSON exactly (camelCase keys, same tag values). The Rust import
  path stays `claude_agent_sdk` regardless of the crate name.
- Add or update a parity test for every wire/type change (see `reference.md`
  "Parity tests"). Tests in `tests/wire_parity.rs` and `tests/type_contracts.rs`
  encode the Python contract.

### 4. Validate

```bash
cargo build
cargo build --features otel
cargo test
```

All must pass. The `e2e_cli` tests are `#[ignore]` (need an authenticated CLI)
and are expected to report 0 run / 4 ignored.

### 5. Update the baseline and summarize

- Edit `UPSTREAM_BASELINE.md` to the new upstream tag + commit you synced to.
- Commit Rust changes and the baseline bump together.
- Report a parity summary: ported items, intentionally-skipped Python-only
  items, and any deferred work.

## Scope rules

- Only port **public behavior/API/wire** parity. Do not mirror Python internal
  structure, asyncio details, or packaging.
- Never weaken existing wire compatibility to make a port easier; if a change is
  ambiguous, add a parity test that pins the Python JSON shape first.
- Keep changes minimal and reviewable; one logical upstream feature per commit
  when practical.

## Resources

- `reference.md` - Python -> Rust module/API map, wire conventions, parity-test
  locations.
- `scripts/fetch_upstream.sh` - cache + diff the upstream repo.
- `UPSTREAM_BASELINE.md` - last-synced upstream version/commit (state file).
