#!/usr/bin/env bash
# Build the registry handler into a component-shaped .wasm.
#
# Mirrors handlers/{echo,sql,markdown}/build.sh. Reads
# ../../../registry/{index,candidates}.json at compile time via
# include_str!  the resulting component carries the registry
# state captured at build time. Rebuild to refresh.
set -euo pipefail
cd "$(dirname "$0")"

ADAPTER="${WASI_ADAPTER:-$HOME/.cache/xtran/wasi_snapshot_preview1.reactor.wasm}"
if [ ! -f "$ADAPTER" ]; then
    echo "wasi adapter not found: $ADAPTER"
    exit 1
fi

# Ensure registry/index.json + registry/candidates.json are current
# before compiling the handler. The component bakes them in via
# include_str!  out-of-date data here means stale output.
(
    cd ../../..
    python3 provenance/build_registry.py > /dev/null
)

cargo build --release
SRC=target/wasm32-wasip2/release/wasm_registry_handler.wasm
OUT=target/wasm32-wasip2/release/wasm_registry_handler.component.wasm
wasm-tools component new "$SRC" --adapt wasi_snapshot_preview1="$ADAPTER" -o "$OUT"
echo "wrote $OUT"
