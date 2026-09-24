#!/usr/bin/env bash
# Build the wasmMachine spec for the REAL sqlite3 shell.
#
# PLAN-wasmmachine.md E3, updated for Route A: this now points at the
# GENUINE upstream sqlite3 shell (deps/sqlite/shell.c) compiled to a
# wasm32-wasip2 `wasi:cli/run` component by scripts/build-shell-wasm.sh,
# not the hand-rolled Rust CLI port (the "lookalike"). The real shell
# statically links sqlite3, so its component imports ONLY the standard
# WASI surface (no sqlite:extension/* host imports) — the spec template
# is sqlite-cli.json.template (kept in sync).
#
# Tool runtime: the sqlink host. End users drive it with
#   sqlink run-tool <component>
# (a real TTY via inherited stdio); the v86 / wasmMachine spec produced
# here describes the same component for the sealed-identity + run phases.
#
# Requires:
#   - the wasi-sdk (scripts/build-shell-wasm.sh resolves it)
#   - wasm-tools (`cargo install wasm-tools`)
#   - blake3sum, OR python3 + the `blake3` package, OR jq + an
#     external `b3sum` tool. We use whichever's on PATH.
#
# v86 tooling (`wasmmachine seal`, `wasmmachine run`) is required
# for the sealed-spec + integration-run phases — those are
# separate (`wasmmachine-seal.sh`, `wasmmachine-run.sh`) and live
# alongside this script.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT/wasmmachine"
# The real-shell component built by scripts/build-shell-wasm.sh.
SHELL_COMPONENT="$ROOT/target/wasm32-wasip2/release/sqlite3-shell.component.wasm"
CLI_COMPONENT="$OUT_DIR/sqlite_cli.component.wasm"
SPEC_TEMPLATE="$OUT_DIR/sqlite-cli.json.template"
SPEC_OUT="$OUT_DIR/sqlite-cli.json"

echo "==> Building the real sqlite3 shell for wasm32-wasip2..."
bash "$ROOT/scripts/build-shell-wasm.sh"

echo "==> Staging real-shell component into wasmmachine/..."
# wasi-sdk 33 emits a component directly; just copy it next to the spec
# so the spec's file:// uri is self-contained.
cp "$SHELL_COMPONENT" "$CLI_COMPONENT"

echo "==> Hashing component with blake3..."
DIGEST_HEX=""
if command -v b3sum >/dev/null 2>&1; then
    DIGEST_HEX="$(b3sum --no-names "$CLI_COMPONENT")"
elif command -v python3 >/dev/null 2>&1 && python3 -c 'import blake3' 2>/dev/null; then
    DIGEST_HEX="$(python3 -c "import sys, blake3; print(blake3.blake3(open(sys.argv[1],'rb').read()).hexdigest())" "$CLI_COMPONENT")"
else
    echo "Error: neither b3sum nor python3+blake3 is available." >&2
    echo "Install with one of:" >&2
    echo "    cargo install b3sum" >&2
    echo "    pip install blake3" >&2
    exit 1
fi
echo "    digest = $DIGEST_HEX"

# v86's spec format expects the digest as an array of 32 bytes
# (see plans/python-v86.json). Convert from hex.
DIGEST_ARRAY="[$(echo "$DIGEST_HEX" | sed 's/\(..\)/\1 /g' | tr ' ' '\n' | grep -v '^$' | sed 's/^/0x/' | xargs printf '%d,' | sed 's/,$//')]"

echo "==> Substituting into spec template..."
sed -e "s|@@CLI_DIGEST_ARRAY@@|$DIGEST_ARRAY|" \
    -e "s|@@CLI_PATH@@|$CLI_COMPONENT|" \
    "$SPEC_TEMPLATE" > "$SPEC_OUT"

echo
echo "Wrote $SPEC_OUT"
echo
echo "Next:"
echo "  wasmmachine seal $SPEC_OUT  # produce the sealed identity"
echo "  wasmmachine run  $SPEC_OUT  # instantiate locally"
echo
echo "Both require ~/git/v86 tooling on PATH. See README's"
echo "'wasmMachine integration' section for end-user usage."
