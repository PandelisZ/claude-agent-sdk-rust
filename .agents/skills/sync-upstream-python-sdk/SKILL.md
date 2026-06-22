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

### 1b. Run the public-surface parity audit (fast triage)

```bash
.agents/skills/sync-upstream-python-sdk/scripts/audit_parity.sh
```

This diffs the upstream public surface (`__init__.py`'s `__all__`) against the
Rust crate and prints only what needs attention:
- **NEW** - upstream symbols neither present in Rust nor recorded as known.
  These are your port candidates. `NEW: 0` means the public surface is at parity.
- **deferred backlog** - known gaps intentionally not yet ported (from
  `parity_known.tsv`); pick these up when asked to go deeper.
- **STALE** - `parity_known.tsv` rows no longer in upstream (clean them up).

The audit is the primary triage tool: it normalizes Python<->Rust naming
(e.g. `Mcp`<->`MCP`) and ignores 1:1 ports, so you only look at real deltas.
Use the `git log`/diff from step 1 to understand *why* each NEW symbol exists
and to catch field-level changes the symbol audit can't see (new fields on an
existing type, changed wire tags, CLI flag tweaks).

### 2. Triage upstream changes

For each NEW symbol and each changed area in the upstream diff, classify it:
- **Public API** (new option, type, message field, function) -> must port.
- **Wire/protocol** (CLI args, control protocol, JSON shapes) -> must port; these
  break parity silently if missed.
- **Behavior/bugfix** -> port if it affects observable behavior.
- **Python-only** (packaging, typing stubs, asyncio plumbing, docs) -> skip, but
  note it in the summary.

Use `reference.md` for the Python-module -> Rust-module map so you edit the
right files. If a NEW symbol is best represented under a different Rust name or
as an enum variant (rather than a 1:1 port), record it in `parity_known.tsv` as
`mapped` so it stops surfacing; if you decide to defer it, record it as
`deferred` with a note on where it should land.

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

- Edit `UPSTREAM_BASELINE.md` to the new upstream tag + commit you synced to,
  and add a one-line entry to its sync log.
- Update `parity_known.tsv`: remove rows for anything you ported to a matching
  name; add `mapped`/`deferred` rows for any NEW symbol you intentionally did
  not port 1:1. Re-run `audit_parity.sh` and confirm NEW is empty (or only
  contains things you consciously deferred).
- Commit Rust changes and the baseline/known-table bumps together.
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
  locations, and known representation differences.
- `scripts/fetch_upstream.sh` - cache + diff the upstream repo.
- `scripts/audit_parity.sh` - compare upstream `__all__` vs the Rust crate;
  prints NEW / deferred / STALE. Run every sync.
- `parity_known.tsv` - table of symbols whose Rust form differs (`mapped`) or
  that are intentionally unported (`deferred`). Keep this current.
- `UPSTREAM_BASELINE.md` - last-synced upstream version/commit + sync log.
