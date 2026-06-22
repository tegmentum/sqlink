#!/bin/bash
# Encode each extension's .wasm artifact into a .component.wasm.
# WASI-Preview1 reactor-shape artifacts are detected and re-encoded
# with the cached wasi_snapshot_preview1 adapter.
#
# Run after every catalog rebuild. Idempotent.
set -e
cd "$(dirname "$0")/.."

ADAPTER="$HOME/.cache/xtran/wasi_snapshot_preview1.reactor.wasm"
if [ ! -f "$ADAPTER" ]; then
    echo "ERROR: adapter not found at $ADAPTER"
    echo "Install wasi_snapshot_preview1.reactor.wasm there first."
    exit 2
fi

ok=0; failed=0
for wasm in $(find target/wasm32-wasip2/release extensions/*/target/wasm32-wasip2/release -maxdepth 1 -name "*.wasm" -not -name "*.component.wasm" 2>/dev/null); do
    out="${wasm%.wasm}.component.wasm"
    # Try plain encode first
    if wasm-tools component new "$wasm" -o "$out" 2>/dev/null; then
        ok=$((ok + 1))
        continue
    fi
    # Fallback: try with the WASI-p1 adapter (for reactor-shape artifacts)
    if wasm-tools component new "$wasm" --adapt "wasi_snapshot_preview1=$ADAPTER" -o "$out" 2>/dev/null; then
        ok=$((ok + 1))
    else
        failed=$((failed + 1))
        echo "FAILED: $(basename $wasm)"
    fi
done

echo "encoded: $ok / failed: $failed"
