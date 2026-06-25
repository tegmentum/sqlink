#!/bin/bash
# Encode each extension's .wasm artifact into a .component.wasm.
# WASI-Preview1 reactor-shape artifacts are detected and re-encoded
# with the cached wasi_snapshot_preview1 adapter.
#
# Run after every catalog rebuild. Idempotent.
#
# WIT-closure tracking: each produced .component.wasm gets a
# <name>.component.wasm.wit-hash sidecar containing the sha256 of
# the union of WIT files it was encoded against. On re-run the
# script compares the recorded hash to the current closure; on
# mismatch (or missing sidecar) it forces a cargo rebuild of the
# extension before re-encoding so the resulting component binds
# against the current SPI surface. Without this guard, stale
# .wasm artifacts left behind from another branch silently
# survive re-encoding and surface as wit-bindgen import-type
# mismatches at test time ("failed to convert function to given
# type").
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

# Decide whether any WIT closure file is newer than the
# .component.wasm at $1. Returns 0 if a newer WIT file exists
# (= possibly skewed, recompute hash), 1 if all WIT files are
# older (= cheap shortcut: safe to skip).
wit_newer_than() {
    local component="$1"
    local pkg="$2"
    local extdir="${PKG_DIR[$pkg]:-}"
    # find -newer with -quit short-circuits at the first hit.
    if find sqlite-loader-wit/wit sqlite-wasm/wit -name "*.wit" -type f -newer "$component" -print -quit 2>/dev/null | grep -q .; then
        return 0
    fi
    if [ -n "$extdir" ] && [ -d "$extdir/wit" ]; then
        if find "$extdir/wit" -name "*.wit" -type f -newer "$component" -print -quit 2>/dev/null | grep -q .; then
            return 0
        fi
    fi
    return 1
}

# Rebuild a package in the right cwd. Top-level workspace
# extensions get `cargo build -p <pkg>` from the repo root;
# per-extension excluded crates (e.g. postgis-bridge, bundle-cli)
# get a directory-local `cargo build` because they're outside the
# workspace.
#
# Args:
#   $1 = cargo package name
#   $2 = source .wasm path (used to decide which target dir owns it)
# Returns 0 on success, non-zero on failure.
rebuild_pkg() {
    local pkg="$1"
    local wasm="$2"
    local extdir="${PKG_DIR[$pkg]:-}"
    # extensions/<dir>/target/... means the package builds out of
    # its own crate dir; everything else lives in the workspace
    # target/ at the repo root.
    case "$wasm" in
        extensions/*/target/*)
            if [ -z "$extdir" ]; then
                return 1
            fi
            ( cd "$REPO_ROOT/$extdir" && cargo build --target wasm32-wasip2 --release >/dev/null 2>&1 )
            ;;
        *)
            ( cd "$REPO_ROOT" && cargo build -p "$pkg" --target wasm32-wasip2 --release >/dev/null 2>&1 )
            ;;
    esac
}

skip=0; rebuild=0; encode=0; failed=0
for wasm in $(find target/wasm32-wasip2/release extensions/*/target/wasm32-wasip2/release -maxdepth 1 -name "*.wasm" -not -name "*.component.wasm" 2>/dev/null); do
    out="${wasm%.wasm}.component.wasm"
    sidecar="$out.wit-hash"

    pkg=$(pkg_from_wasm "$(basename "$wasm")")

    # Perf shortcut: if the sidecar exists, the component exists,
    # AND no WIT file is newer than the component, the artifact
    # is up to date — skip everything.
    if [ -f "$sidecar" ] && [ -f "$out" ] && ! wit_newer_than "$out" "$pkg"; then
        echo "[skip]    $pkg (up-to-date)"
        skip=$((skip + 1))
        continue
    fi

    # Otherwise compute current WIT-closure hash and decide.
    wit_hash=$(compute_wit_hash "$pkg")
    sidecar_hash=""
    [ -f "$sidecar" ] && sidecar_hash=$(cat "$sidecar" 2>/dev/null)

    if [ "$wit_hash" != "$sidecar_hash" ]; then
        # Sidecar missing or mismatched -> WIT closure changed
        # (or unknown). Rebuild before re-encoding so the next
        # `component new` reads bindgen output from the current WIT.
        if [ -z "$sidecar_hash" ]; then
            echo "[rebuild] $pkg (no sidecar  unknown provenance)"
        else
            echo "[rebuild] $pkg (WIT skew)"
        fi
        if ! rebuild_pkg "$pkg" "$wasm"; then
            failed=$((failed + 1))
            echo "[fail]    $pkg (cargo build failed)"
            continue
        fi
        rebuild=$((rebuild + 1))
        # The rebuild may have produced a new .wasm at the same
        # path; loop expression already captured the path, fall
        # through to encode.
    else
        echo "[encode]  $pkg (sidecar match  re-encode only)"
        encode=$((encode + 1))
    fi

    # Try plain encode first
    if wasm-tools component new "$wasm" -o "$out" 2>/dev/null; then
        printf '%s' "$wit_hash" > "$sidecar"
        continue
    fi
    # Fallback: try with the WASI-p1 adapter (for reactor-shape artifacts)
    if wasm-tools component new "$wasm" --adapt "wasi_snapshot_preview1=$ADAPTER" -o "$out" 2>/dev/null; then
        printf '%s' "$wit_hash" > "$sidecar"
    else
        failed=$((failed + 1))
        echo "[fail]    $pkg (wasm-tools component new failed)"
    fi
done

echo
echo "summary: skip=$skip rebuild=$rebuild encode=$encode failed=$failed"
