#!/usr/bin/env bash
# Build the wasmMachine spec for sqlite-cli.
#
# PLAN-wasmmachine.md E3. Compiles the cli to a wasm32-wasip2
# component, hashes it with blake3, substitutes the digest + path
# into wasmmachine/sqlite-cli.json.template, and writes the result
# to wasmmachine/sqlite-cli.json.
#
# Requires:
#   - cargo, rustc with wasm32-wasip2 target
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
CLI_RELEASE="$ROOT/target/wasm32-wasip2/release/sqlite_cli.wasm"
CLI_COMPONENT="$OUT_DIR/sqlite_cli.component.wasm"
SPEC_TEMPLATE="$OUT_DIR/sqlite-cli.json.template"
SPEC_OUT="$OUT_DIR/sqlite-cli.json"

echo "==> Building sqlite-cli for wasm32-wasip2..."
(cd "$ROOT" && cargo build -p sqlite-cli --target wasm32-wasip2 --release)

echo "==> Wrapping core module as wasi-p2 component..."
wasm-tools component new "$CLI_RELEASE" -o "$CLI_COMPONENT"

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
