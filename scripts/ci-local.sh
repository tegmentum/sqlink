#!/usr/bin/env bash
# Run a GitHub Actions workflow locally via nektos/act.
#
# Usage:
#   scripts/ci-local.sh <workflow>           [extra act args...]
#   scripts/ci-local.sh ci                   # host-side checks
#   scripts/ci-local.sh wasm-smoke           # wasm build + extension-smoke
#   scripts/ci-local.sh fuzz-smoke           # 5 cargo-fuzz targets
#   scripts/ci-local.sh mutants-nightly      # cargo-mutants (long)
#   scripts/ci-local.sh --list               # show available workflows
#   scripts/ci-local.sh --help               # this message
#
# Reads .actrc at the repo root for image / arch defaults.
# Requires Docker + nektos/act installed (`brew install act`).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

usage() {
    sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
}

list_workflows() {
    echo "Available workflows:"
    for wf in .github/workflows/*.yml; do
        name="$(basename "$wf" .yml)"
        trigger="$(grep -E '^  (push|pull_request|schedule|workflow_dispatch):' "$wf" \
                   | head -1 | sed -E 's/^  ([a-z_]+):.*/\1/')"
        printf "  %-22s  (trigger: %s)\n" "$name" "${trigger:-?}"
    done
}

if [[ $# -eq 0 ]] || [[ "${1:-}" == "--help" ]] || [[ "${1:-}" == "-h" ]]; then
    usage; exit 0
fi

if [[ "${1:-}" == "--list" ]] || [[ "${1:-}" == "-l" ]]; then
    list_workflows; exit 0
fi

WORKFLOW="$1"; shift
WF_FILE=".github/workflows/${WORKFLOW}.yml"

if [[ ! -f "$WF_FILE" ]]; then
    echo "error: workflow '$WORKFLOW' not found at $WF_FILE" >&2
    echo >&2
    list_workflows >&2
    exit 2
fi

if ! command -v act >/dev/null 2>&1; then
    echo "error: act not installed. On macOS: brew install act" >&2
    echo "       Cross-platform: gh extension install nektos/gh-act" >&2
    exit 3
fi

if ! docker info >/dev/null 2>&1; then
    echo "error: Docker is not running. act needs a docker-compatible daemon." >&2
    echo "       Start Docker Desktop, OrbStack, or colima before retrying." >&2
    exit 4
fi

# Pick the event act should simulate based on the workflow's `on:` clause.
# Schedule-driven workflows need an explicit `schedule` event payload;
# `act schedule` synthesizes a minimal one. Push/PR workflows just need
# the matching event name.
EVENT="push"
if grep -qE '^  pull_request:' "$WF_FILE"; then
    EVENT="pull_request"
fi
if grep -qE '^  schedule:' "$WF_FILE"; then
    EVENT="schedule"
fi

echo "==> running act:"
echo "    workflow: $WF_FILE"
echo "    event:    $EVENT"
echo "    extras:   $*"
echo

exec act "$EVENT" -W "$WF_FILE" "$@"
