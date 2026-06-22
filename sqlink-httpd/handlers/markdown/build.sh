#!/usr/bin/env bash
# Build the markdown handler into a component-shaped .wasm.
# Mirrors handlers/echo/build.sh; see it for the rationale.
set -euo pipefail
cd "$(dirname "$0")"

ADAPTER="${WASI_ADAPTER:-$HOME/.cache/xtran/wasi_snapshot_preview1.reactor.wasm}"
if [ ! -f "$ADAPTER" ]; then
    echo "wasi adapter not found: $ADAPTER"
    echo "set WASI_ADAPTER or fetch wasi-preview1-component-adapter"
    exit 1
fi

cargo build --release
SRC=target/wasm32-wasip2/release/wasm_markdown_handler.wasm
OUT=target/wasm32-wasip2/release/wasm_markdown_handler.component.wasm
wasm-tools component new "$SRC" --adapt wasi_snapshot_preview1="$ADAPTER" -o "$OUT"
echo "wrote $OUT"
