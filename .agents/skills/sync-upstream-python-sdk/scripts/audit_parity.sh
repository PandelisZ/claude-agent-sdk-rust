#!/usr/bin/env bash
#
# Parity audit: compare the upstream Python SDK public surface (__init__.__all__)
# against this Rust crate, and report only the symbols that need attention.
#
# Output buckets:
#   NEW       upstream symbol that is neither auto-matched in Rust source nor
#             listed in parity_known.tsv -> investigate and port (or record).
#   STALE     parity_known.tsv entry no longer in upstream __all__ -> clean up.
#   deferred  count of known, intentionally-unported gaps (from parity_known.tsv).
#
# A symbol "auto-matches" when its normalized name (lowercased, non-alphanumerics
# stripped) equals a normalized identifier found anywhere in src/. That covers
# 1:1 ports and Mcp<->MCP casing. Symbols that map to a different Rust shape
# (e.g. a `ContentBlock` enum variant) won't auto-match; record those in
# parity_known.tsv as `mapped` so they stop showing up as NEW.
#
# Usage:
#   audit_parity.sh            # uses the cached upstream checkout (see fetch_upstream.sh)
#   UPSTREAM_SDK_CACHE=/path audit_parity.sh

set -euo pipefail

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE_ROOT="$(cd "${SKILL_DIR}/../../.." && pwd)"
CACHE_DIR="${UPSTREAM_SDK_CACHE:-${TMPDIR:-/tmp}/claude-agent-sdk-python}"
INIT_PY="${CACHE_DIR}/src/claude_agent_sdk/__init__.py"
KNOWN_TSV="${SKILL_DIR}/parity_known.tsv"

if [ ! -f "${INIT_PY}" ]; then
  echo "Upstream checkout not found at ${CACHE_DIR}." >&2
  echo "Run scripts/fetch_upstream.sh first (it clones/updates the cache)." >&2
  exit 1
fi

python3 - "${INIT_PY}" "${CRATE_ROOT}" "${KNOWN_TSV}" <<'PY'
import ast, os, re, sys

init_py, crate_root, known_tsv = sys.argv[1], sys.argv[2], sys.argv[3]

def norm(s: str) -> str:
    return re.sub(r"[^a-z0-9]", "", s.lower())

# 1. Upstream public surface from __all__.
tree = ast.parse(open(init_py).read())
upstream = []
for node in ast.walk(tree):
    if isinstance(node, ast.Assign) and any(
        isinstance(t, ast.Name) and t.id == "__all__" for t in node.targets
    ):
        upstream = [e.value for e in node.value.elts if isinstance(e, ast.Constant)]
upstream = [s for s in upstream if s != "__version__"]  # maps to VERSION; always present

# 2. Normalized identifier set from Rust source (identifiers + serde rename strings).
rust_ids = set()
src_dir = os.path.join(crate_root, "src")
for root, _, files in os.walk(src_dir):
    for f in files:
        if not f.endswith(".rs"):
            continue
        text = open(os.path.join(root, f), encoding="utf-8").read()
        for tok in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", text):
            rust_ids.add(norm(tok))
        for lit in re.findall(r'"([^"]+)"', text):  # serde rename tags
            rust_ids.add(norm(lit))

# 3. Known-mapping table: symbol \t status(mapped|deferred) \t note.
known = {}
if os.path.exists(known_tsv):
    for line in open(known_tsv):
        line = line.rstrip("\n")
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        sym = parts[0].strip()
        status = parts[1].strip() if len(parts) > 1 else "mapped"
        note = parts[2].strip() if len(parts) > 2 else ""
        known[sym] = (status, note)

new, deferred = [], []
for sym in upstream:
    if norm(sym) in rust_ids:
        continue  # auto-matched 1:1
    if sym in known:
        if known[sym][0] == "deferred":
            deferred.append((sym, known[sym][1]))
        continue  # mapped/deferred -> accounted for
    new.append(sym)

upstream_set = set(upstream)
stale = [s for s in known if s not in upstream_set]

print(f"upstream __all__ symbols: {len(upstream)} (+__version__)")
print(f"auto-matched in Rust:     {len(upstream) - len(new) - sum(1 for s in upstream if s in known)}")
print(f"known (mapped+deferred):  {len(known)}")
print()

print(f"== NEW (investigate / port): {len(new)} ==")
for s in new:
    print(f"  {s}")
if not new:
    print("  (none - crate is at parity with the upstream public surface)")

print()
print(f"== deferred backlog: {len(deferred)} ==")
for s, note in deferred:
    print(f"  {s}  -- {note}")

if stale:
    print()
    print(f"== STALE parity_known.tsv entries (no longer upstream): {len(stale)} ==")
    for s in stale:
        print(f"  {s}")
PY
