#!/usr/bin/env bash
# Phase 1 (#227): build a compose:dynlink resident provider (<ext>-provider.wasm)
# for every registry extension. Single-tier -> full provider (0 leftover imports);
# multi-tier -> flagged (one-tier-per-artifact limit). Runs from the main tree
# (which has the generated source a fresh worktree lacks).
set -uo pipefail
R=/Users/zacharywhitley/git/sqlink
PROVSRC=${PROVSRC:-/Users/zacharywhitley/git/woco-220-wt/sqlite-extension-endpoint/provider}
ROLLOUT="${PROVIDER_OUT:-$HOME/.cache/sqlink/provider-rollout}"
mkdir -p "$ROLLOUT" /tmp/p1-shapes
LOG="$ROLLOUT/rollout.log"; : > "$LOG"

echo "building 8 provider shapes..." | tee -a "$LOG"
for s in scalar aggregate collation vtab vtab-mut hooks hooks-noauth dotcmd; do
  ( cd "$PROVSRC" && cargo build --release --target wasm32-wasip2 --no-default-features --features "$s" >/dev/null 2>&1 \
    && cp target/wasm32-wasip2/release/sqlite_extension_endpoint.wasm "/tmp/p1-shapes/$s.wasm" ) \
    && echo "  shape $s ok" >>"$LOG" || echo "  shape $s FAIL" >>"$LOG"
done

iface_to_shape() {
  case "$1" in
    scalar-function) echo scalar;; aggregate-function) echo aggregate;;
    collation) echo collation;; vtab|module) echo vtab;;
    commit-hook|update-hook|rollback-hook) echo hooks;; dotcmd) echo dotcmd;;
    *) echo "";; esac
}

NAMES=$(python3 -c "import json; d=json.load(open('$R/registry/index.json')); print('\n'.join(e['name'] for e in d['extensions'] if e.get('checksum','') not in ('sha256:builtin','sha256:unbuilt')))")
total=$(echo "$NAMES" | grep -c .)
echo "extensions to roll out: $total" | tee -a "$LOG"

ok=0; multi=0; plugfail=0; buildfail=0; notier=0; i=0
for name in $NAMES; do
  i=$((i+1)); u=$(echo "$name" | tr - _)
  extdir="$R/extensions/$name"
  [ -d "$extdir" ] || { echo "[$i/$total] SKIP $name (no dir)" >>"$LOG"; continue; }
  # Build the ext CORE with plain `cargo build` (not `cargo component build`,
  # which emits a finished component whose inner core Binaryen can't touch),
  # `wasm-opt -Os` it, then wrap it into a component with wasm-tools — so the
  # shipped provider ships smaller (mirrors smoke-build-provider.sh + make ext).
  ( cd "$extdir" && timeout 300 cargo build --release --target wasm32-wasip2 >/dev/null 2>&1 )
  core="$R/target/wasm32-wasip2/release/${u}_extension.wasm"
  [ -f "$core" ] || core="$extdir/target/wasm32-wasip2/release/${u}_extension.wasm"
  if [ ! -f "$core" ]; then echo "[$i/$total] BUILDFAIL $name" >>"$LOG"; buildfail=$((buildfail+1)); continue; fi
  bash "$R/tooling/wasm-opt-core.sh" "$core" >/dev/null 2>&1
  ADAPTER="${WASI_ADAPTER:-$HOME/.cache/xtran/wasi_snapshot_preview1.reactor.wasm}"
  comp="${core%.wasm}.component.wasm"
  wasm-tools component new "$core" --adapt "wasi_snapshot_preview1=$ADAPTER" -o "$comp" 2>/dev/null \
    || wasm-tools component new "$core" -o "$comp" 2>/dev/null \
    || { echo "[$i/$total] BUILDFAIL $name (component new)" >>"$LOG"; buildfail=$((buildfail+1)); continue; }
  # The shape is determined by the tiers the ext EXPORTS (implements), not the
  # interfaces it IMPORTS (host-satisfied deps like wal-frames/spi/session). Reading
  # imports here mis-selected the hook tier (e.g. wal-archive imports wal-frames but
  # exports only update/commit/wal-hook, never authorizer).
  ifaces=$(wasm-tools component wit "$comp" 2>/dev/null \
    | grep -E '^  export sqlite:extension/' \
    | sed -E 's#^  export sqlite:extension/([a-z0-9-]+).*#\1#' | sort -u)
  # shapes are hierarchical (each supersets scalar); pick the SUPERSET shape by precedence.
  shape=""
  echo "$ifaces" | grep -qx vtab-update && shape=vtab-mut
  [ -z "$shape" ] && echo "$ifaces" | grep -qx vtab && shape=vtab
  [ -z "$shape" ] && echo "$ifaces" | grep -qx dot-command && shape=dotcmd
  # hook tier: authorizer export => full hooks; update/commit/wal-hook without an
  # authorizer export => hooks-noauth (avoids a leftover authorizer import).
  if [ -z "$shape" ] && echo "$ifaces" | grep -qE '^(authorizer|update-hook|commit-hook|wal-hook)$'; then
    if echo "$ifaces" | grep -qx authorizer; then shape=hooks; else shape=hooks-noauth; fi
  fi
  [ -z "$shape" ] && echo "$ifaces" | grep -qx aggregate-function && shape=aggregate
  [ -z "$shape" ] && echo "$ifaces" | grep -qx collation && shape=collation
  [ -z "$shape" ] && echo "$ifaces" | grep -qx scalar-function && shape=scalar
  if [ -z "$shape" ]; then echo "[$i/$total] NOTIER $name (ifaces: $(echo $ifaces))" >>"$LOG"; notier=$((notier+1)); continue; fi
  wac plug --plug "$comp" "/tmp/p1-shapes/$shape.wasm" -o "$ROLLOUT/$name-provider.wasm" >/dev/null 2>&1
  left=$(wasm-tools component wit "$ROLLOUT/$name-provider.wasm" 2>/dev/null | grep -E '^  import sqlite:extension/' | grep -vE 'types|policy|cli-' | wc -l | tr -d ' ')
  if [ -f "$ROLLOUT/$name-provider.wasm" ] && [ "${left:-1}" = "0" ]; then
    echo "[$i/$total] OK $name ($shape)" >>"$LOG"; ok=$((ok+1))
  else
    echo "[$i/$total] PLUGFAIL $name (shape=$shape leftover=${left:-?})" >>"$LOG"; plugfail=$((plugfail+1))
  fi
done
echo "=== DONE: OK=$ok MULTITIER=$multi PLUGFAIL=$plugfail BUILDFAIL=$buildfail NOTIER=$notier of $total ===" | tee -a "$LOG"
