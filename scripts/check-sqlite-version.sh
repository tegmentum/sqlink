#!/usr/bin/env bash
#
# check-sqlite-version.sh  fail if deps/sqlite/sqlite3.h and the
# libsqlite3-sys crate resolved in Cargo.lock disagree on
# SQLITE_VERSION.
#
# Two sqlite3 amalgamations flow into a full build:
#   1. deps/sqlite/sqlite3.c  fetched by scripts/download-sqlite.sh
#      and compiled into every C-built Makefile target
#      (sqlite.wasm, extensions).
#   2. The `sqlite3/` amalgamation vendored inside libsqlite3-sys,
#      compiled by cargo when the wasm cli component is built with
#      the `bundled` feature.
#
# Drift lets `SELECT sqlite_version()` report different strings from
# the two artifact families. This script fails loudly the moment they
# stop matching.
#
# Requires that libsqlite3-sys has been unpacked into the cargo
# registry  run `cargo fetch` first if the crate cache is empty.
set -euo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
CARGO_HOME_DIR=${CARGO_HOME:-$HOME/.cargo}

DEPS_SQLITE_H=$REPO_ROOT/deps/sqlite/sqlite3.h
if [ ! -f "$DEPS_SQLITE_H" ]; then
    echo "$0: $DEPS_SQLITE_H missing  run 'make sqlite' first" >&2
    exit 2
fi

# Cargo.lock is authoritative for the resolved libsqlite3-sys version.
# Grab the version from the block that starts with `name =
# "libsqlite3-sys"`.
LOCK=$REPO_ROOT/Cargo.lock
LIBSQLITE3_SYS_VER=$(awk '
    /^\[\[package\]\]/ { in_pkg = 1; name = ""; ver = ""; next }
    in_pkg && /^name = / {
        gsub(/"/, "", $3); name = $3
    }
    in_pkg && /^version = / {
        gsub(/"/, "", $3); ver = $3
    }
    in_pkg && name == "libsqlite3-sys" && ver != "" {
        print ver; exit
    }
' "$LOCK")

if [ -z "${LIBSQLITE3_SYS_VER-}" ]; then
    echo "$0: libsqlite3-sys not found in $LOCK" >&2
    exit 3
fi

# Registry layout: $CARGO_HOME/registry/src/<index-hash>/libsqlite3-sys-X.Y.Z/.
# The <index-hash> path varies by cargo version, so glob.
shopt -s nullglob
CRATE_ROOTS=("$CARGO_HOME_DIR"/registry/src/*/libsqlite3-sys-"$LIBSQLITE3_SYS_VER")
if [ ${#CRATE_ROOTS[@]} -eq 0 ]; then
    echo "$0: libsqlite3-sys-$LIBSQLITE3_SYS_VER not unpacked in $CARGO_HOME_DIR/registry/src/*"  >&2
    echo "     run 'cargo fetch' from $REPO_ROOT first" >&2
    exit 4
fi
LIBSQLITE3_SYS_H=${CRATE_ROOTS[0]}/sqlite3/sqlite3.h
if [ ! -f "$LIBSQLITE3_SYS_H" ]; then
    echo "$0: expected $LIBSQLITE3_SYS_H, not found" >&2
    exit 5
fi

_version_of() {
    # `#define SQLITE_VERSION        "3.53.2"` -> `3.53.2`.
    awk '/^#define[ \t]+SQLITE_VERSION[ \t]/ {
        gsub(/"/, "", $3); print $3; exit
    }' "$1"
}

DEPS_VER=$(_version_of "$DEPS_SQLITE_H")
BUNDLED_VER=$(_version_of "$LIBSQLITE3_SYS_H")

printf "  deps/sqlite:               %s  (%s)\n" "$DEPS_VER" "$DEPS_SQLITE_H"
printf "  libsqlite3-sys %s bundle: %s  (%s)\n" \
    "$LIBSQLITE3_SYS_VER" "$BUNDLED_VER" "$LIBSQLITE3_SYS_H"

if [ "$DEPS_VER" != "$BUNDLED_VER" ]; then
    cat >&2 <<EOF

sqlite version drift: $DEPS_VER (make-side) vs $BUNDLED_VER (cargo-side).

    Fix by bumping SQLITE_VERSION in scripts/download-sqlite.sh to
    $BUNDLED_VER  (SQLITE_VERSION="$(echo "$BUNDLED_VER" | awk -F. '{ printf "%d%02d%02d00\n", $1, $2, $3 }')"),
    then 'rm -rf deps/sqlite && make sqlite' locally.
EOF
    exit 1
fi

echo "OK  both sides on SQLite $DEPS_VER"
