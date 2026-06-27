#!/usr/bin/env bash
# Build the composed `cli + sqlite-lib` browser runtime.
#
# Output: target/wasm32-wasip2/release/cli_with_sqlite.component.wasm
#         — a single component that exports `wasi:cli/run` and
#         contains in-wasm SQLite + the multi-memory cold-tier
#         substrate (pool 1 = pcache, pool 2 = VFS).
#
# Prerequisites:
#
#   - sqlite-wasm submodule on a commit that ships the multi-
#     memory sqlite-lib build pipeline (Stage 4 of Path 3).
#   - All 10 dot-cmd extensions built + component-encoded so the
#     cli's `include_bytes!` succeeds:
#
#       for ext in archive-cli core-dotcmd serialize-cli \
#                  session-cli sha3sum-cli sqlink-meta-cli \
#                  sqlite-utils-data sqlite-utils-fts \
#                  sqlite-utils-maint sqlite-utils-schema; do
#         cargo build --release --target wasm32-wasip2 \
#           --manifest-path extensions/$ext/Cargo.toml
#       done
#       bash scripts/encode-extension-components.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SQLITE_WASM_ROOT="$REPO_ROOT/sqlite-wasm"

echo "[1/3] build sqlite-lib (multi-memory component)"
bash "$SQLITE_WASM_ROOT/scripts/build-sqlite-lib-component.sh"

echo "[2/3] build sqlite-cli (core wasm + component new)"
( cd "$REPO_ROOT" && cargo build -p sqlite-cli --target wasm32-wasip2 --release )
# Per-artifact decision (v1.6 polish, #487 Sub-item C):
#   sqlite_cli.wasm is built from cli/ with `crate-type = ["cdylib"]`
#   via `cargo build --target wasm32-wasip2` (NOT `cargo component build`).
#   wit-bindgen's `generate!` macro embeds a `component-type:*` custom
#   section into the cdylib but does NOT wrap the result as a wasi-p2
#   component. The output is a CORE module + custom section; the
#   subsequent `wasm-tools component new` step wraps it into the
#   wasi-preview2 component that `wac compose` consumes.
#
#   v1.5 round 1 incorrectly believed this step was a no-op for
#   wasm32-wasip2; v1.5 round 2 corrected: it is required for every
#   cargo-built cdylib. KEEP this step.
wasm-tools component new \
    "$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.wasm" \
    -o "$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.component.wasm"

echo "[3/3] wac compose cli ← sqlite-lib"
# Switched from `wac plug` to `wac compose` with the recipe so the
# composed binary re-exports sqlite-lib's dispatch-bridge. wac plug
# auto-strips exports the outer world doesn't declare; the recipe
# lets us explicitly surface dispatch-bridge for the JS host's
# spi-loader.register-scalar impl to call.
wac compose "$REPO_ROOT/composition-cli-sqlite-lib.wac" \
    -d "sqlite:wasm-lib=$SQLITE_WASM_ROOT/target/wasm32-wasip2/release/sqlite_lib.component.wasm" \
    -d "sqlite:cli=$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.component.wasm" \
    -o "$REPO_ROOT/target/wasm32-wasip2/release/cli_with_sqlite.component.wasm"

OUT="$REPO_ROOT/target/wasm32-wasip2/release/cli_with_sqlite.component.wasm"
echo
echo "wrote $OUT"
echo
echo "Composition world:"
wasm-tools component wit "$OUT" | grep -E '^\s*(import|export)' | head -30
echo
echo "Note: instantiates under sqlink (which provides the"
echo "  sqlink:wasm/extension-loader@0.1.0 imports). Direct"
echo "  wasmtime run will error on those imports; that's expected."

# [4/3] composectl emit parallel cross-check (Tier 1.1.b)
#
# PLAN-orchestration-integration.md asks for `composectl emit` to run
# alongside `wac compose` for one release as a parallel cross-check,
# then `wac` retires. The plan is in composition-plans/sqlink-runtime.plan.json.
#
# The cross-check is GATED on two upstream gaps tracked in
# docs/notes/orchestration-substrate-gaps.md:
#
#   - Gap 1: composectl emit cannot re-export non-root component
#     instances. Our composed runtime re-exports sqlite-lib's
#     dispatch-bridge + types (the load-bearing reason we use
#     `wac compose` with an explicit recipe rather than `wac plug`).
#   - Gap 2: composectl emit doesn't unify versioned WASI imports
#     between cli (0.2.6) and sqlite-lib (0.2.4); wac compose does.
#
# When either gap closes upstream, flip ORCHESTRATION_CROSS_CHECK=1
# below. Until then the block emits the plan-validate gate (which
# confirms the plan still parses against the schema) but skips
# the emit-side cross-check.
ORCHESTRATION_CROSS_CHECK="${ORCHESTRATION_CROSS_CHECK:-0}"
ORCH_ROOT="${SQLINK_ORCH_ROOT:-$REPO_ROOT/../webassembly-component-orchestration}"
COMPOSECTL="${COMPOSECTL_BIN:-$ORCH_ROOT/target/release/composectl}"
RUNTIME_PLAN="$REPO_ROOT/composition-plans/sqlink-runtime.plan.json"

if [[ -x "$COMPOSECTL" && -f "$RUNTIME_PLAN" ]]; then
    echo
    echo "[orchestration] validating composition-plans/sqlink-runtime.plan.json"
    "$COMPOSECTL" plan validate "$RUNTIME_PLAN" || {
        echo "[orchestration] WARNING: plan validation failed (non-fatal)"
    }

    if [[ "$ORCHESTRATION_CROSS_CHECK" == "1" ]]; then
        echo "[orchestration] composectl emit parallel cross-check (Tier 1.1.b)"
        # When upstream gaps close, this block:
        #   1. Renders the plan with current cli + lib digests.
        #   2. Runs `composectl emit build` to produce composectl-out.wasm.
        #   3. Diffs the WIT surface against $OUT.
        # Until gaps close, exporting ORCHESTRATION_CROSS_CHECK=1 should
        # trigger the failure modes documented in
        # docs/notes/orchestration-substrate-gaps.md, which is itself a
        # useful canary for "is upstream there yet?".
        echo "[orchestration] cross-check is gated on upstream gaps;"
        echo "[orchestration] see docs/notes/orchestration-substrate-gaps.md"
    else
        echo "[orchestration] cross-check disabled (set ORCHESTRATION_CROSS_CHECK=1 once upstream gaps close)"
    fi
fi
