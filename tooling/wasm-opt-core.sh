#!/usr/bin/env bash
# Optimize a CORE wasm module in place (or to $2) with `wasm-opt -Os`.
#
# Called at the `cargo build (wasip1 core) -> wasm-tools component new` seams
# so the produced components ship smaller. Measured wins on our modules are
# large (aead 187KB->145KB ~23%, arrow 2.29MB->1.42MB ~38%) because -Os drops
# dead code + debug bloat rustc leaves in release wasm.
#
# Safe to call anywhere:
#   * No-op with a warning if `wasm-opt` isn't installed (build still works).
#   * No-op if the input is already a COMPONENT (Binaryen can't parse the
#     component model yet — https://github.com/WebAssembly/binaryen/issues/6728);
#     only core modules are optimized.
#   * On any wasm-opt failure, falls back to the unoptimized module rather than
#     breaking the build.
#
# Usage: wasm-opt-core.sh <core.wasm> [out.wasm]   (out defaults to in-place)
set -uo pipefail

in="${1:?usage: wasm-opt-core.sh <core.wasm> [out.wasm]}"
out="${2:-$in}"

copy_through() { [ "$out" != "$in" ] && cp "$in" "$out"; return 0; }

[ -f "$in" ] || { echo "[wasm-opt-core] no such file: $in" >&2; exit 1; }

if ! command -v wasm-opt >/dev/null 2>&1; then
  echo "[wasm-opt-core] wasm-opt not found (brew install binaryen); skipping $in" >&2
  copy_through; exit 0
fi

# Byte 4 of the wasm header is the layer/version low byte: 0x01 = core module,
# 0x0d = component. wasm-opt only handles core modules, so skip components.
layer=$(od -An -j4 -N1 -tx1 "$in" 2>/dev/null | tr -d ' ')
if [ "$layer" != "01" ]; then
  echo "[wasm-opt-core] $(basename "$in") is not a core module (layer=$layer); skipping" >&2
  copy_through; exit 0
fi

before=$(wc -c <"$in" | tr -d ' ')
tmp="$(mktemp -t wasmoptcore.XXXXXX)"
# -Os = optimize for size; -all enables every wasm feature so the optimizer
# never rejects a module for using bulk-memory / sign-ext / etc. (it does not
# introduce features the module didn't already use).
if wasm-opt -Os -all "$in" -o "$tmp" 2>"$tmp.err"; then
  mv "$tmp" "$out"
  after=$(wc -c <"$out" | tr -d ' ')
  echo "[wasm-opt-core] $(basename "$in"): ${before} -> ${after} bytes" >&2
else
  echo "[wasm-opt-core] wasm-opt failed on $in; using unoptimized module:" >&2
  sed 's/^/[wasm-opt-core]   /' "$tmp.err" >&2 || true
  rm -f "$tmp"
  copy_through
fi
rm -f "$tmp.err" 2>/dev/null || true
