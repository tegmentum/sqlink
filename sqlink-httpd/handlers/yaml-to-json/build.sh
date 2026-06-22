#!/usr/bin/env bash
# Build the yaml-to-json handler into a component-shaped .wasm.
#
# wasm32-wasip2 by itself emits a core module; we wrap with
# wasm-tools component new + the wasi-p1 reactor adapter to get
# a true component. Same pipeline as the echo / sql handlers.
#
# Outputs (under target/wasm32-wasip2/release/):
#   wasm_yaml_to_json_handler.wasm            core module (intermediate)
#   wasm_yaml_to_json_handler.component.wasm  the component you --load
set -euo pipefail
cd "$(dirname "$0")"

ADAPTER="${WASI_ADAPTER:-$HOME/.cache/xtran/wasi_snapshot_preview1.reactor.wasm}"
if [ ! -f "$ADAPTER" ]; then
    echo "wasi adapter not found: $ADAPTER"
    echo "set WASI_ADAPTER, or fetch from"
    echo "  https://github.com/bytecodealliance/wasmtime/releases (wasi-preview1-component-adapter)"
    exit 1
fi

cargo build --release
SRC=target/wasm32-wasip2/release/wasm_yaml_to_json_handler.wasm
OUT=target/wasm32-wasip2/release/wasm_yaml_to_json_handler.component.wasm
wasm-tools component new "$SRC" --adapt wasi_snapshot_preview1="$ADAPTER" -o "$OUT"
echo "wrote $OUT"
