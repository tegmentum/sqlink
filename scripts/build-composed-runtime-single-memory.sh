#!/usr/bin/env bash
# Build the composed `cli + sqlite-lib` runtime  single-memory flavor.
#
# Output: target/wasm32-wasip2/release/cli_with_sqlite.single_memory.component.wasm
#         a single component that exports `wasi:cli/run` and contains
#         in-wasm SQLite with the InProc HashMap/Vec<u8> cold tiers
#         (NOT the multi-memory substrate). One linear memory total,
#         which is what jco can transpile today.
#
# This is the browser target. The native wasmtime runtime (scenarios
# 1 + 2) keeps using build-composed-runtime.sh, which composes the
# multi-memory sqlite-lib (256 MiB-per-pool capacity).
#
# Prerequisites:
#
#   - sqlite-wasm submodule on a commit that ships the
#     `single-memory` Cargo feature on sqlite-lib + sqlite-pcache-tvm
#     + sqlite-vfs-tvm.
#   - All 10 dot-cmd extensions already built + component-encoded so
#     the cli's `include_bytes!` succeeds. See build-composed-runtime.sh
#     for the list.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SQLITE_WASM_ROOT="$REPO_ROOT/sqlite-wasm"

echo "[1/3] build sqlite-lib (single-memory component)"
bash "$SQLITE_WASM_ROOT/scripts/build-sqlite-lib-component-single-memory.sh"

# The cli bakes the dot-command extension PROVIDERS via include_bytes! (#220:
# plugged <name>-provider.wasm, not the raw component). Build them first, else
# the include_bytes! paths are missing at cli compile time.
echo "[1.5/3] build embedded dot-command providers"
bash "$REPO_ROOT/tooling/build-embedded-providers.sh"

echo "[2/3] build sqlite-cli (core wasm + component new)"
# cargo wbuild = `cargo build --target wasm32-wasip2` + wasm-only
# `-DSQLITE_THREADSAFE=0` layered on via --config env override
# (defined in workspace .cargo/config.toml). #823.
( cd "$REPO_ROOT" && cargo wbuild -p sqlite-cli --release )
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
# component (the only seam where the core is exposed). No-op if wasm-opt is
# missing or the artifact is already a component.
bash "$REPO_ROOT/tooling/wasm-opt-core.sh" \
    "$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.wasm"
wasm-tools component new \
    "$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.wasm" \
    -o "$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.component.wasm"

echo "[3/3] wac compose cli  sqlite-lib (single-memory)"
# Switched from `wac plug` to `wac compose` with an explicit recipe
# so the composed binary re-exports sqlite-lib's dispatch-bridge.
# wac plug auto-strips exports the outer world doesn't declare;
# the recipe lets us explicitly surface dispatch-bridge for the JS
# host's spi-loader.register-scalar impl to call.
wac compose "$REPO_ROOT/composition-cli-sqlite-lib.wac" \
    -d "sqlite:wasm-lib=$SQLITE_WASM_ROOT/target/wasm32-wasip2/release/sqlite_lib.single_memory.component.wasm" \
    -d "sqlite:cli=$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.component.wasm" \
    -o "$REPO_ROOT/target/wasm32-wasip2/release/cli_with_sqlite.single_memory.component.wasm"

OUT="$REPO_ROOT/target/wasm32-wasip2/release/cli_with_sqlite.single_memory.component.wasm"
echo
echo "wrote $OUT"
echo
echo "Composition world:"
wasm-tools component wit "$OUT" | grep -E '^\s*(import|export)' | head -30
echo
echo "Note: still expects sqlink:wasm/extension-loader imports from"
echo "the host (sqlink native or browser polyfill); a direct wasmtime"
echo "run will error on those imports."

# [4/3] composectl emit parallel cross-check (Tier 1.1.b)
#
# Same shape as build-composed-runtime.sh. The single-memory variant
# uses the same composition recipe (composition-cli-sqlite-lib.wac)
# and therefore the same composition-plans/sqlink-runtime.plan.json.
# Upstream gaps are resolved as of webassembly-component-orchestration
# 2e3ee85f + a7a5a809 (Gap 1/2) and 58ce66f0 (Gap 3); see
# docs/notes/orchestration-substrate-gaps.md.
SQLINK_COMPOSE_TOOL="${SQLINK_COMPOSE_TOOL:-wac}"
if [[ "${ORCHESTRATION_CROSS_CHECK:-0}" == "1" && "$SQLINK_COMPOSE_TOOL" == "wac" ]]; then
    SQLINK_COMPOSE_TOOL="both"
fi
ORCH_ROOT="${SQLINK_ORCH_ROOT:-$REPO_ROOT/../webassembly-component-orchestration}"
COMPOSECTL="${COMPOSECTL_BIN:-$ORCH_ROOT/target/release/composectl}"
RUNTIME_PLAN="$REPO_ROOT/composition-plans/sqlink-runtime.plan.json"

emit_via_composectl() {
    local out_path="$1"
    local cli_path="$REPO_ROOT/target/wasm32-wasip2/release/sqlite_cli.component.wasm"
    local lib_path="$SQLITE_WASM_ROOT/target/wasm32-wasip2/release/sqlite_lib.single_memory.component.wasm"
    local rendered_plan
    rendered_plan="$(mktemp -t sqlink-runtime-plan.XXXXXX.json)"

    local cli_digest lib_digest
    cli_digest="$("$COMPOSECTL" blob put "$cli_path" 2>/dev/null | awk '/Digest:/ {print $2}')"
    lib_digest="$("$COMPOSECTL" blob put "$lib_path" 2>/dev/null | awk '/Digest:/ {print $2}')"
    if [[ -z "$cli_digest" || -z "$lib_digest" ]]; then
        echo "[orchestration] failed to stage inputs in composectl blob store" >&2
        rm -f "$rendered_plan"
        return 1
    fi

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

    COMPOSECTL_OUT="$REPO_ROOT/target/wasm32-wasip2/release/cli_with_sqlite.single_memory.composectl.component.wasm"
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
            cp "$COMPOSECTL_OUT" "$OUT"
            echo "[orchestration] promoted composectl artifact to $OUT"
        fi
    else
        echo "[orchestration] composectl emit failed; wac artifact remains authoritative"
    fi
fi
