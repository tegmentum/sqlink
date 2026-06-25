#!/bin/bash
# Encode each extension's .wasm artifact into a .component.wasm.
# WASI-Preview1 reactor-shape artifacts are detected and re-encoded
# with the cached wasi_snapshot_preview1 adapter.
#
# Run after every catalog rebuild. Idempotent.
#
# WIT-closure tracking: each produced .component.wasm gets a
# <name>.component.wasm.wit-hash sidecar containing the sha256 of
# the union of WIT files it was encoded against. The sidecar is
# the durable record of WIT provenance for that artifact.
set -e
cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

ADAPTER="$HOME/.cache/xtran/wasi_snapshot_preview1.reactor.wasm"
if [ ! -f "$ADAPTER" ]; then
    echo "ERROR: adapter not found at $ADAPTER"
    echo "Install wasi_snapshot_preview1.reactor.wasm there first."
    exit 2
fi

# Hash backend: prefer sha256sum, fall back to shasum, then openssl.
if command -v sha256sum >/dev/null 2>&1; then
    HASHER="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
    HASHER="shasum -a 256"
elif command -v openssl >/dev/null 2>&1; then
    HASHER="openssl dgst -sha256 -r"
else
    echo "ERROR: no sha256 hasher (sha256sum/shasum/openssl) on PATH" >&2
    exit 2
fi

# Build pkg-name -> extension-dir map once (cargo package name as
# declared in Cargo.toml; dir is whatever the user named it).
declare -A PKG_DIR
for dir in extensions/*/; do
    [ -f "$dir/Cargo.toml" ] || continue
    pkg=$(grep -m1 -E '^name = "' "$dir/Cargo.toml" 2>/dev/null \
        | sed 's/name = "\(.*\)"/\1/')
    [ -n "$pkg" ] || continue
    PKG_DIR["$pkg"]="${dir%/}"
done

# Canonical workspace SPI WIT — shared by every extension that
# imports the host SPI (almost all of them).
CANONICAL_WIT=()
for f in $(find sqlite-loader-wit/wit -name "*.wit" -type f 2>/dev/null | sort); do
    CANONICAL_WIT+=("$f")
done
for f in $(find sqlite-wasm/wit -name "*.wit" -type f 2>/dev/null | sort); do
    CANONICAL_WIT+=("$f")
done

# Compute WIT-closure hash for a package. The closure is:
#   1. Canonical workspace WIT (sqlite-loader-wit/wit/, sqlite-wasm/wit/)
#   2. Extension's own wit/ tree if present (this also covers
#      any locally-vendored wit/deps/ since they live underneath).
#
# Args:
#   $1 = cargo package name (e.g. "uuid-extension")
# Stdout: hex sha256 with no trailing newline.
compute_wit_hash() {
    local pkg="$1"
    local extdir="${PKG_DIR[$pkg]:-}"
    local files=("${CANONICAL_WIT[@]}")
    if [ -n "$extdir" ] && [ -d "$extdir/wit" ]; then
        local f
        for f in $(find "$extdir/wit" -name "*.wit" -type f 2>/dev/null | sort); do
            files+=("$f")
        done
    fi
    # Concat in sorted-path order and hash. `cat` over empty list
    # would consume stdin; guard with the size check.
    if [ "${#files[@]}" -eq 0 ]; then
        printf '%s' "0000000000000000000000000000000000000000000000000000000000000000"
        return
    fi
    cat "${files[@]}" 2>/dev/null | $HASHER | awk '{printf "%s", $1}'
}

# Derive cargo package name from a .wasm basename. Cargo's
# wasm32-wasip2 output replaces `-` with `_` in the package name;
# inverting that gives back the package name.
pkg_from_wasm() {
    local base="$1"
    base="${base%.wasm}"
    echo "${base//_/-}"
}

ok=0; failed=0
for wasm in $(find target/wasm32-wasip2/release extensions/*/target/wasm32-wasip2/release -maxdepth 1 -name "*.wasm" -not -name "*.component.wasm" 2>/dev/null); do
    out="${wasm%.wasm}.component.wasm"
    sidecar="$out.wit-hash"

    # Compute the WIT-closure hash for this artifact now so we
    # can record it on a successful encode. Skew detection +
    # rebuild lands in the follow-up commit.
    pkg=$(pkg_from_wasm "$(basename "$wasm")")
    wit_hash=$(compute_wit_hash "$pkg")

    # Try plain encode first
    if wasm-tools component new "$wasm" -o "$out" 2>/dev/null; then
        printf '%s' "$wit_hash" > "$sidecar"
        ok=$((ok + 1))
        continue
    fi
    # Fallback: try with the WASI-p1 adapter (for reactor-shape artifacts)
    if wasm-tools component new "$wasm" --adapt "wasi_snapshot_preview1=$ADAPTER" -o "$out" 2>/dev/null; then
        printf '%s' "$wit_hash" > "$sidecar"
        ok=$((ok + 1))
    else
        failed=$((failed + 1))
        echo "FAILED: $(basename $wasm)"
    fi
done

echo "encoded: $ok / failed: $failed"
