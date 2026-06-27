#!/usr/bin/env bash
# Build the REAL sqlite3 shell (deps/sqlite/shell.c) to a
# wasm32-wasip2 `wasi:cli/run` command COMPONENT.
#
# This is the sqlink mirror of ducklink's build-shell-ext-wasm.sh:
# instead of the hand-rolled Rust CLI port (the "lookalike"), we ship
# the genuine upstream sqlite3 shell — its dot-commands, output modes
# (.mode box/json/csv/...), and completion — compiled to wasm and run
# through the sqlink host via `sqlink run-tool`.
#
# Output: target/wasm32-wasip2/release/sqlite3-shell.component.wasm
#
# Notes on the wasip2 build:
#   * wasi-sdk 33 (clang 22) emits a component DIRECTLY for the
#     wasm32-wasip2 target — the linked artifact already exports
#     `wasi:cli/run` and imports the standard wasi:cli/io/clocks/
#     filesystem set. No `wasm-tools component new` + adapter step.
#   * No linenoise / readline / editline: WASI preview2 has no termios,
#     so we build with HAVE_READLINE=0 HAVE_EDITLINE=0 HAVE_LINENOISE=0
#     and the shell falls back to its built-in local_getline() line
#     reader (reads stdin via WASI, full interactive + piped input).
#   * Signals / process clocks / getpid: supplied by the wasi-sdk
#     emulation libraries. system() (the .shell/.system dot-commands)
#     has no emulation lib, so scripts/shell-wasi-shims.c stubs it.
#
# Requires: wasi-sdk (clang for wasm32-wasip2). Resolved from, in order:
#   $WASI_SDK / ~/wasi-sdk / deps/wasi-sdk-33.0-arm64-macos.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SQLITE_DIR="$REPO_ROOT/deps/sqlite"
SHIMS_C="$REPO_ROOT/scripts/shell-wasi-shims.c"
OUT_DIR="$REPO_ROOT/target/wasm32-wasip2/release"
OUT="$OUT_DIR/sqlite3-shell.component.wasm"

# --- resolve wasi-sdk -------------------------------------------------
if [[ -n "${WASI_SDK:-}" && -x "$WASI_SDK/bin/clang" ]]; then
    WASI="$WASI_SDK"
elif [[ -x "$HOME/wasi-sdk/bin/clang" ]]; then
    WASI="$HOME/wasi-sdk"
elif [[ -x "$REPO_ROOT/deps/wasi-sdk-33.0-arm64-macos/bin/clang" ]]; then
    WASI="$REPO_ROOT/deps/wasi-sdk-33.0-arm64-macos"
else
    echo "error: wasi-sdk not found. Set \$WASI_SDK or run scripts/download-wasi-sdk.sh" >&2
    exit 1
fi
CLANG="$WASI/bin/clang"
SYSROOT="$WASI/share/wasi-sysroot"
echo "==> wasi-sdk: $WASI"
echo "    $("$CLANG" --version | head -1)"

# --- ensure sqlite amalgamation present -------------------------------
if [[ ! -f "$SQLITE_DIR/shell.c" || ! -f "$SQLITE_DIR/sqlite3.c" ]]; then
    echo "==> sqlite amalgamation missing; downloading..."
    bash "$REPO_ROOT/scripts/download-sqlite.sh"
fi

mkdir -p "$OUT_DIR"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

TARGET_FLAGS="--sysroot=$SYSROOT -target wasm32-wasip2"

# SQLite library defines: kept aligned with this repo's LIBSQLITE3_FLAGS
# (.cargo/config.toml) so the shell's bundled sqlite3 behaves like the
# host's. LOAD_EXTENSION is ENABLED — the shell's `.load` is the Route A
# entry point bridged to the host's wasm-component loader (P2).
SQLITE_DEFS=(
    -DSQLITE_THREADSAFE=0
    -DSQLITE_OMIT_DEPRECATED
    -DSQLITE_ENABLE_MATH_FUNCTIONS
    -DSQLITE_ENABLE_FTS5
    -DSQLITE_ENABLE_RTREE
    -DSQLITE_ENABLE_GEOPOLY
    -DSQLITE_ENABLE_DBSTAT_VTAB
    -DSQLITE_ENABLE_BYTECODE_VTAB
    -DSQLITE_ENABLE_STMTVTAB
    -DSQLITE_ENABLE_SESSION
    -DSQLITE_ENABLE_PREUPDATE_HOOK
    -DSQLITE_ENABLE_DESERIALIZE
    -DSQLITE_ENABLE_LOAD_EXTENSION
    -DSQLITE_DEFAULT_MEMSTATUS=0
    -DSQLITE_LIKE_DOESNT_MATCH_BLOBS
    -DSQLITE_MAX_EXPR_DEPTH=0
    -DSQLITE_USE_ALLOCA
)

# Shell defines: no line-editing libs (no termios on wasip2 -> built-in
# local_getline fallback); the WASI POSIX-emulation toggles.
SHELL_DEFS=(
    -DHAVE_READLINE=0
    -DHAVE_EDITLINE=0
    -DHAVE_LINENOISE=0
    -D_WASI_EMULATED_SIGNAL
    -D_WASI_EMULATED_PROCESS_CLOCKS
    -D_WASI_EMULATED_GETPID
)

echo "==> compile sqlite3.c"
"$CLANG" $TARGET_FLAGS -O2 "${SQLITE_DEFS[@]}" \
    -c "$SQLITE_DIR/sqlite3.c" -o "$WORK/sqlite3.o"

echo "==> compile shell.c"
"$CLANG" $TARGET_FLAGS -O2 "${SQLITE_DEFS[@]}" "${SHELL_DEFS[@]}" \
    -c "$SQLITE_DIR/shell.c" -o "$WORK/shell.o"

echo "==> compile wasi shims"
"$CLANG" $TARGET_FLAGS -O2 -c "$SHIMS_C" -o "$WORK/shims.o"

echo "==> link -> wasi:cli/run component"
"$CLANG" $TARGET_FLAGS -O2 \
    "$WORK/shell.o" "$WORK/sqlite3.o" "$WORK/shims.o" \
    -lwasi-emulated-signal \
    -lwasi-emulated-process-clocks \
    -lwasi-emulated-getpid \
    -lm \
    -o "$OUT"

echo
echo "wrote $OUT ($(wc -c < "$OUT") bytes)"

if command -v wasm-tools >/dev/null 2>&1; then
    echo
    echo "Component world:"
    wasm-tools component wit "$OUT" | grep -E '^\s*(import|export)' || true
    echo
    echo "Run it:  sqlink run-tool $OUT"
fi
