#!/bin/bash
# Smoke test for scripts/encode-extension-components.sh skew
# detection. Runs four checks against uuid-extension:
#
#   1. After a clean encode run, the WIT-hash sidecar exists.
#   2. A no-op re-run reports [skip] for uuid.
#   3. After a WIT-closure content change, the next run reports
#      [rebuild] for uuid and rewrites the sidecar to a new hash.
#   4. After reverting the WIT change and re-running, the sidecar
#      hash returns to the original.
#
# Runtime warning: check 3 forces a workspace-wide cargo rebuild
# (changing host-spi.wit invalidates every bindgen consumer).
# Expect several minutes on first run; subsequent runs are
# faster because cargo's incremental cache covers most crates.
set -e
cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

UUID_WASM="target/wasm32-wasip2/release/uuid_extension.wasm"
UUID_COMP="target/wasm32-wasip2/release/uuid_extension.component.wasm"
UUID_HASH="target/wasm32-wasip2/release/uuid_extension.component.wasm.wit-hash"
WIT_FILE="sqlite-loader-wit/wit/host-spi.wit"
MARKER="// encode-skew-smoke marker — should be reverted"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}
pass() {
    echo "PASS: $*"
}

cleanup() {
    # Revert the WIT marker if the test bailed mid-flight. The
    # submodule may be on a detached HEAD/branch; use checkout
    # at the path, not at the submodule level.
    if [ -d sqlite-loader-wit ]; then
        ( cd sqlite-loader-wit && git checkout -- wit/host-spi.wit 2>/dev/null || true )
    fi
}
trap cleanup EXIT

echo "=== Setup ==="
echo "Ensuring uuid-extension is built and encoded..."
cargo build -p uuid-extension --target wasm32-wasip2 --release >/dev/null 2>&1 \
    || fail "uuid-extension build failed"
bash scripts/encode-extension-components.sh >/dev/null 2>&1 \
    || fail "initial encode run failed"

echo
echo "=== Check 1: sidecar exists after encode ==="
[ -f "$UUID_HASH" ] || fail "sidecar $UUID_HASH does not exist"
ORIGINAL_HASH=$(cat "$UUID_HASH")
[ ${#ORIGINAL_HASH} -eq 64 ] \
    || fail "sidecar content is not a 64-char hex hash: ${ORIGINAL_HASH}"
pass "sidecar exists with hash ${ORIGINAL_HASH:0:16}..."

echo
echo "=== Check 2: no-op re-run reports [skip] ==="
NOOP_OUTPUT=$(bash scripts/encode-extension-components.sh 2>&1)
echo "$NOOP_OUTPUT" | grep -q "\[skip\]    uuid-extension" \
    || fail "no-op re-run did not [skip] uuid-extension. output snippet:
$(echo "$NOOP_OUTPUT" | grep uuid | head -3)"
pass "no-op re-run skipped uuid-extension"

echo
echo "=== Check 3: WIT content change triggers [rebuild] ==="
echo "(Expected runtime: several minutes — workspace-wide rebuild)"
echo "$MARKER" >> "$WIT_FILE"
SKEW_OUTPUT=$(bash scripts/encode-extension-components.sh 2>&1)
echo "$SKEW_OUTPUT" | grep -q "\[rebuild\] uuid-extension" \
    || fail "WIT-skew run did not [rebuild] uuid-extension. output snippet:
$(echo "$SKEW_OUTPUT" | grep uuid | head -3)"
SKEW_HASH=$(cat "$UUID_HASH")
[ "$SKEW_HASH" != "$ORIGINAL_HASH" ] \
    || fail "sidecar hash unchanged after WIT change (was $ORIGINAL_HASH, still $SKEW_HASH)"
pass "WIT-skew run rebuilt uuid-extension; sidecar rotated to ${SKEW_HASH:0:16}..."

echo
echo "=== Check 4: revert returns sidecar to original hash ==="
( cd sqlite-loader-wit && git checkout -- wit/host-spi.wit )
echo "(Expected runtime: several minutes — workspace-wide rebuild back)"
REVERT_OUTPUT=$(bash scripts/encode-extension-components.sh 2>&1)
REVERTED_HASH=$(cat "$UUID_HASH")
[ "$REVERTED_HASH" = "$ORIGINAL_HASH" ] \
    || fail "sidecar hash did not return to original after revert (was $ORIGINAL_HASH, now $REVERTED_HASH)"
pass "post-revert sidecar matches original hash"

echo
echo "=== All checks passed ==="
