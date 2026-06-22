#!/usr/bin/env bash
#
# Fetch/update a cached checkout of the upstream Python Claude Agent SDK and
# show what changed since our last-synced baseline.
#
# Usage:
#   fetch_upstream.sh [REF]
#
#   REF  Optional git ref (tag/branch/sha) to sync to. Defaults to the latest
#        release tag if available, otherwise the default branch HEAD.
#
# Reads the last-synced commit from UPSTREAM_BASELINE.md (the line beginning
# with "commit:"). Prints the upstream version, the baseline, and the diff
# (git log + changed files) between them.

set -euo pipefail

REPO_URL="https://github.com/anthropics/claude-agent-sdk-python.git"
CACHE_DIR="${UPSTREAM_SDK_CACHE:-${TMPDIR:-/tmp}/claude-agent-sdk-python}"
SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASELINE_FILE="${SKILL_DIR}/UPSTREAM_BASELINE.md"
REQUESTED_REF="${1:-}"

echo "== Upstream Python SDK sync =="
echo "repo:  ${REPO_URL}"
echo "cache: ${CACHE_DIR}"

# Clone or update the cache.
if [ -d "${CACHE_DIR}/.git" ]; then
  git -C "${CACHE_DIR}" fetch --tags --force --prune origin >/dev/null 2>&1
else
  git clone --quiet "${REPO_URL}" "${CACHE_DIR}"
fi

# Resolve the target ref.
if [ -n "${REQUESTED_REF}" ]; then
  TARGET_REF="${REQUESTED_REF}"
else
  TARGET_REF="$(git -C "${CACHE_DIR}" tag --sort=-version:refname | head -n1 || true)"
  if [ -z "${TARGET_REF}" ]; then
    TARGET_REF="origin/HEAD"
  fi
fi

git -C "${CACHE_DIR}" checkout --quiet --detach "${TARGET_REF}" 2>/dev/null || \
  git -C "${CACHE_DIR}" checkout --quiet "${TARGET_REF}"

CURRENT_SHA="$(git -C "${CACHE_DIR}" rev-parse HEAD)"
CURRENT_DESC="$(git -C "${CACHE_DIR}" describe --tags --always 2>/dev/null || echo "${CURRENT_SHA}")"

# Read baseline commit from the state file.
BASELINE_SHA=""
if [ -f "${BASELINE_FILE}" ]; then
  BASELINE_SHA="$(grep -E '^commit:' "${BASELINE_FILE}" | head -n1 | sed -E 's/^commit:[[:space:]]*//' | tr -d '[:space:]')"
fi

echo
echo "target ref:     ${TARGET_REF}"
echo "upstream now:   ${CURRENT_DESC} (${CURRENT_SHA})"
echo "synced baseline: ${BASELINE_SHA:-<none recorded>}"
echo

if [ -z "${BASELINE_SHA}" ]; then
  echo "No baseline recorded yet. Treat this as the initial parity audit."
  echo "After syncing, set UPSTREAM_BASELINE.md to commit ${CURRENT_SHA}."
  exit 0
fi

if [ "${BASELINE_SHA}" = "${CURRENT_SHA}" ]; then
  echo "Already at baseline. Nothing to sync."
  exit 0
fi

if ! git -C "${CACHE_DIR}" cat-file -e "${BASELINE_SHA}^{commit}" 2>/dev/null; then
  echo "WARNING: baseline ${BASELINE_SHA} not found in upstream history."
  echo "Inspect manually; the cache may need a full fetch."
  exit 0
fi

echo "== Commits since baseline =="
git -C "${CACHE_DIR}" log --oneline --no-decorate "${BASELINE_SHA}..${CURRENT_SHA}"

echo
echo "== Changed files since baseline =="
git -C "${CACHE_DIR}" diff --stat "${BASELINE_SHA}..${CURRENT_SHA}"

echo
echo "To see a focused diff, run e.g.:"
echo "  git -C '${CACHE_DIR}' diff ${BASELINE_SHA}..${CURRENT_SHA} -- src/claude_agent_sdk/types.py"
