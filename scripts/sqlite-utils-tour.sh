#!/usr/bin/env bash
# Walk every shipped sqlite-utils dot command against a fresh db.
# Builds the cli + utils extensions if needed; produces the JSON
# fixture; runs the tour; reports pass/fail.
#
# Usage: scripts/sqlite-utils-tour.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

DB=/tmp/sqlite-utils-tour.db
FIXTURE=/tmp/sqlite-utils-tour-dogs.json
TOUR=examples/sqlite-utils-tour.sql
CLI_BIN=target/release/sqlink
CLI_COMPONENT=target/wasm32-wasip2/release/sqlite_cli.component.wasm

if [[ ! -x "$CLI_BIN" ]]; then
    echo "missing $CLI_BIN  build with: cargo build --release" >&2
    exit 1
fi
if [[ ! -f "$CLI_COMPONENT" ]]; then
    echo "missing $CLI_COMPONENT  build with:" >&2
    echo "  cargo build -p sqlite-cli --target wasm32-wasip2 --release" >&2
    echo "  wasm-tools component new target/wasm32-wasip2/release/sqlite_cli.wasm -o $CLI_COMPONENT" >&2
    exit 1
fi

rm -f "$DB"
cat >"$FIXTURE" <<'EOF'
[
  {"id": 1, "name": "Cleo",     "breed": "labrador", "age": 4},
  {"id": 2, "name": "Pancakes", "breed": "poodle",   "age": 2},
  {"id": 3, "name": "Otto",     "breed": "corgi",    "age": 7}
]
EOF

echo "==> running tour: $TOUR"
"$CLI_BIN" --db "$DB" "$CLI_COMPONENT" <"$TOUR"
echo
echo "==> tour completed; final dogs count:"
"$CLI_BIN" --db "$DB" "$CLI_COMPONENT" <<'EOF'
SELECT count(*) FROM dogs;
EOF
