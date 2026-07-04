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
# Shrink the cli CORE module with `wasm-opt -Os` before wrapping it into a
# component (Binaryen can't touch the finished component; this is the only
# seam where the core is exposed). No-op if wasm-opt is missing or the artifact
# is already a component.
bash "$REPO_ROOT/tooling/wasm-opt-core.sh" \
    "$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.wasm"
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
# then `wac` retires. The plan lives at
# composition-plans/sqlink-runtime.plan.json and now carries
# `explicit_exports` for sqlite-lib's dispatch-bridge + types (Gap 1
# resolved upstream in webassembly-component-orchestration
# 2e3ee85f + a7a5a809). Blob-store 1 GiB ceiling was raised in
# 58ce66f0. Version-unification (Gap 2) rides along on the
# wac-graph library emit path.
#
# Choose the composition tool via SQLINK_COMPOSE_TOOL:
#   - `wac`       (default) — wac compose the recipe (preserves the
#                 pre-substrate behavior; safe fallback).
#   - `composectl` — emit from the declarative plan; wac still runs
#                 first so the artifact is always produced even if
#                 emit fails.
#   - `both`      — run both, then diff WIT surfaces (parallel
#                 cross-check per Tier 1.1.b).
SQLINK_COMPOSE_TOOL="${SQLINK_COMPOSE_TOOL:-wac}"
# Legacy alias: `ORCHESTRATION_CROSS_CHECK=1` maps to `both`.
if [[ "${ORCHESTRATION_CROSS_CHECK:-0}" == "1" && "$SQLINK_COMPOSE_TOOL" == "wac" ]]; then
    SQLINK_COMPOSE_TOOL="both"
fi
ORCH_ROOT="${SQLINK_ORCH_ROOT:-$REPO_ROOT/../webassembly-component-orchestration}"
COMPOSECTL="${COMPOSECTL_BIN:-$ORCH_ROOT/target/release/composectl}"
RUNTIME_PLAN="$REPO_ROOT/composition-plans/sqlink-runtime.plan.json"

emit_via_composectl() {
    local out_path="$1"
    local cli_path="$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.component.wasm"
    local lib_path="$SQLITE_WASM_ROOT/target/wasm32-wasip2/release/sqlite_lib.component.wasm"
    local rendered_plan
    rendered_plan="$(mktemp -t sqlink-runtime-plan.XXXXXX.json)"

    # Stage inputs in the blob store and render the plan with current digests.
    local cli_digest lib_digest
    cli_digest="$("$COMPOSECTL" blob put "$cli_path" 2>/dev/null | awk '/Digest:/ {print $2}')"
    lib_digest="$("$COMPOSECTL" blob put "$lib_path" 2>/dev/null | awk '/Digest:/ {print $2}')"
    if [[ -z "$cli_digest" || -z "$lib_digest" ]]; then
        echo "[orchestration] failed to stage inputs in composectl blob store" >&2
        rm -f "$rendered_plan"
        return 1
    fi

    # Render the plan template: swap sqlite-cli + sqlite-lib digests to
    # the freshly computed hex-string values. Plan digests are encoded
    # as byte arrays; convert hex -> JSON array via python3.
    python3 - "$RUNTIME_PLAN" "$cli_digest" "$lib_digest" >"$rendered_plan" <<'PY'
import json, sys
plan_path, cli_hex, lib_hex = sys.argv[1], sys.argv[2], sys.argv[3]
def hex_to_bytes(h):
    return [int(h[i:i+2], 16) for i in range(0, len(h), 2)]
plan = json.load(open(plan_path))
mapping = {"sqlite-cli": hex_to_bytes(cli_hex), "sqlite-lib": hex_to_bytes(lib_hex)}
for c in plan["components"]:
    if c["id"] in mapping:
        c["digest"] = mapping[c["id"]]
json.dump(plan, sys.stdout, indent=2)
PY

    "$COMPOSECTL" emit build "$rendered_plan" --output "$out_path"
    local rc=$?
    rm -f "$rendered_plan"
    return $rc
}

if [[ ! -x "$COMPOSECTL" || ! -f "$RUNTIME_PLAN" ]]; then
    if [[ "$SQLINK_COMPOSE_TOOL" != "wac" ]]; then
        echo "[orchestration] composectl not found at $COMPOSECTL; falling back to wac"
        SQLINK_COMPOSE_TOOL="wac"
    fi
fi

if [[ "$SQLINK_COMPOSE_TOOL" != "wac" ]]; then
    echo
    echo "[orchestration] validating composition-plans/sqlink-runtime.plan.json"
    "$COMPOSECTL" plan validate "$RUNTIME_PLAN"

    COMPOSECTL_OUT="$REPO_ROOT/target/wasm32-wasip2/release/cli_with_sqlite.composectl.component.wasm"
    echo "[orchestration] composectl emit -> $COMPOSECTL_OUT"
    if emit_via_composectl "$COMPOSECTL_OUT"; then
        if [[ "$SQLINK_COMPOSE_TOOL" == "both" ]]; then
            echo "[orchestration] diffing wac vs composectl WIT surfaces"
            diff -u \
                <(wasm-tools component wit "$OUT"             | grep -E '^\s*(import|export) ' | sort -u) \
                <(wasm-tools component wit "$COMPOSECTL_OUT"  | grep -E '^\s*(import|export) ' | sort -u) \
                && echo "[orchestration] WIT surface parity: OK" \
                || echo "[orchestration] WIT surface parity: DIFF (see above)"
        elif [[ "$SQLINK_COMPOSE_TOOL" == "composectl" ]]; then
            # Promote the composectl artifact to the canonical name so
            # downstream steps pick it up.
            cp "$COMPOSECTL_OUT" "$OUT"
            echo "[orchestration] promoted composectl artifact to $OUT"
        fi
    else
        echo "[orchestration] composectl emit failed; wac artifact remains authoritative"
    fi
fi
