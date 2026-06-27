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
