#!/usr/bin/env bash
# Build a `<ext>-provider.wasm` for the smoke harness (#220: the CLI loads a
# plugged compose:dynlink provider, not the raw ext component). Invoked by the
# smoke engine as `smoke.build_argv` with the package name, e.g. `zstd-extension`.
#
# Steps: build the ext component -> infer its shape from its exports -> wac plug
# the matching woco provider shape -> write `<underscore>_provider.wasm` into the
# workspace target dir (the engine then copies it to smoke.extensions_dir as
# `<name>-provider.wasm` per smoke.artifact_name). Mirrors deploy/providers/rollout.sh.
set -uo pipefail
R="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="${1:?usage: smoke-build-provider.sh <package-name>}"
BARE="${PKG%-extension}"                 # zstd-extension -> zstd
U="${BARE//-/_}"                         # wal-archive -> wal_archive
PROVSRC="${PROVSRC:-$HOME/git/woco-220-wt/sqlite-extension-endpoint/provider}"
TARGET_DIR="$R/target/wasm32-wasip2/release"
SHAPES="${SMOKE_SHAPES_DIR:-/tmp/p1-shapes}"
mkdir -p "$SHAPES"

extdir="$R/extensions/$BARE"
[ -d "$extdir" ] || { echo "no extension dir: extensions/$BARE" >&2; exit 1; }

# 1. Build the ext component (the workspace .cargo/config supplies the wasi-sdk
#    CC env for C-linking exts like compress).
( cd "$extdir" && cargo component build --release --target wasm32-wasip2 ) >&2 || exit 1
# Standalone-workspace exts (e.g. zstd) build to extensions/<name>/target;
# root-workspace exts (changeset, compress) build to the root target.
comp=""
for d in "$extdir/target/wasm32-wasip2/release" "$TARGET_DIR"; do
  for cand in "$d/${U}_extension.wasm" "$d/${U}_extension.component.wasm"; do
    [ -f "$cand" ] && comp="$cand" && break 2
  done
done
[ -n "$comp" ] || { echo "ext component not found for $BARE (looked in $extdir/target + $TARGET_DIR)" >&2; exit 1; }

# 2. Infer the shape from the interfaces the ext EXPORTS (not imports).
ifaces=$(wasm-tools component wit "$comp" 2>/dev/null \
  | grep -E '^  export sqlite:extension/' \
  | sed -E 's#^  export sqlite:extension/([a-z0-9-]+).*#\1#' | sort -u)
shape=""
echo "$ifaces" | grep -qx vtab-update && shape=vtab-mut
[ -z "$shape" ] && echo "$ifaces" | grep -qx vtab && shape=vtab
[ -z "$shape" ] && echo "$ifaces" | grep -qx dot-command && shape=dotcmd
if [ -z "$shape" ] && echo "$ifaces" | grep -qE '^(authorizer|update-hook|commit-hook|wal-hook)$'; then
  if echo "$ifaces" | grep -qx authorizer; then shape=hooks; else shape=hooks-noauth; fi
fi
[ -z "$shape" ] && echo "$ifaces" | grep -qx aggregate-function && shape=aggregate
[ -z "$shape" ] && echo "$ifaces" | grep -qx collation && shape=collation
[ -z "$shape" ] && echo "$ifaces" | grep -qx scalar-function && shape=scalar
[ -z "$shape" ] && { echo "could not infer shape for $BARE (ifaces: $ifaces)" >&2; exit 1; }

# 3. Build the shape from the woco provider if not already cached.
if [ ! -f "$SHAPES/$shape.wasm" ]; then
  [ -d "$PROVSRC" ] || { echo "woco provider source not found: $PROVSRC (set PROVSRC)" >&2; exit 1; }
  ( cd "$PROVSRC" && cargo build --release --target wasm32-wasip2 --no-default-features --features "$shape" ) >&2 \
    && cp "$PROVSRC/target/wasm32-wasip2/release/sqlite_extension_endpoint.wasm" "$SHAPES/$shape.wasm" \
    || { echo "failed to build shape $shape" >&2; exit 1; }
fi

# 4. Plug -> the root workspace target dir (build_output; engine then copies it
#    to smoke.extensions_dir as <name>-provider.wasm).
mkdir -p "$TARGET_DIR"
out="$TARGET_DIR/${U}_provider.wasm"
wac plug --plug "$comp" "$SHAPES/$shape.wasm" -o "$out" >&2 || { echo "wac plug failed" >&2; exit 1; }
echo "built $BARE ($shape) -> $out" >&2
