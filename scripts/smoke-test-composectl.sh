#!/usr/bin/env bash
# Smoke-test the orchestration dependency.
#
# Tier 0.3 of PLAN-orchestration-integration.md. The minimum-viable
# check that sqlink can drive `composectl` end-to-end:
#
#   1. Locate the sibling `webassembly-component-orchestration` repo
#      (the dependency model picked in Tier 0.1; see
#      `docs/notes/orchestration-dependency.md`).
#   2. Build composectl if it isn't already built.
#   3. Run `plan validate` against the orchestration repo's own
#      canonical CBOR conformance fixtures.
#
# This is the lightweight equivalent of the path-dep + compile-time
# crate test the plan also mentions. We keep it as a shell test rather
# than a Cargo workspace member because composectl is consumed as a
# binary tool (not a library) by sqlink's build scripts: the same
# composectl invocations that this script smokes are the ones
# `scripts/build-composed-runtime*.sh` will gain in Tier 1.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Sibling repo location, overridable for CI / non-standard checkouts.
ORCH_ROOT="${SQLINK_ORCH_ROOT:-$REPO_ROOT/../webassembly-component-orchestration}"

if [[ ! -d "$ORCH_ROOT" ]]; then
    echo "ERROR: orchestration repo not found at $ORCH_ROOT" >&2
    echo "       Set SQLINK_ORCH_ROOT to override, or clone" >&2
    echo "       https://github.com/zacharywhitley/webassembly-component-orchestration" >&2
    echo "       as a sibling checkout next to this sqlink clone." >&2
    exit 2
fi

COMPOSECTL="${COMPOSECTL_BIN:-$ORCH_ROOT/target/release/composectl}"

if [[ ! -x "$COMPOSECTL" ]]; then
    echo "[smoke] composectl not built; building release binary..."
    ( cd "$ORCH_ROOT" && cargo build --release --bin composectl )
fi

echo "[smoke] composectl: $COMPOSECTL"
"$COMPOSECTL" --help > /dev/null

# The orchestration repo's conformance vectors are canonical CBOR
# plans. `plan validate` parses + validates schema only (no blob
# staging required), which is exactly the linkage check we want.
VECTORS_DIR="$ORCH_ROOT/conformance/vectors"
PASS=0
FAIL=0
for vector in "$VECTORS_DIR"/hello-plan.cbor \
              "$VECTORS_DIR"/nested-plan.cbor \
              "$VECTORS_DIR"/large-int-plan.cbor \
              "$VECTORS_DIR"/multi-component-plan.cbor; do
    if "$COMPOSECTL" plan validate "$vector" 2>&1 | tail -1 | grep -q "Plan is valid"; then
        echo "[smoke] PASS: $(basename "$vector")"
        PASS=$((PASS + 1))
    else
        echo "[smoke] FAIL: $(basename "$vector")"
        FAIL=$((FAIL + 1))
    fi
done

# Negative cases: plans that intentionally fail validation.
for vector in "$VECTORS_DIR"/multi-component-plan-unsorted.cbor \
              "$VECTORS_DIR"/duplicate-plan.cbor; do
    if "$COMPOSECTL" plan validate "$vector" > /dev/null 2>&1; then
        echo "[smoke] FAIL: $(basename "$vector") was expected to be rejected"
        FAIL=$((FAIL + 1))
    else
        echo "[smoke] PASS (rejected as expected): $(basename "$vector")"
        PASS=$((PASS + 1))
    fi
done

echo
echo "[smoke] $PASS passed, $FAIL failed"
if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
