#!/usr/bin/env bash
# Integration test for the ./sqlite-wasm shell wrapper.
#
# Exercises every invocation shape and asserts on stdout. Run
# directly:
#
#     ./tests/cli/sqlite-wasm-wrapper.sh
#
# Or under cargo test via host/tests/sqlite_wasm_wrapper.rs.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
wrapper="$repo_root/sqlite-wasm"
component="$repo_root/target/wasm32-wasip2/release/sqlite_cli.component.wasm"
runner="$repo_root/target/debug/sqlite-wasm-run"

if [[ ! -f "$component" ]]; then
    echo "skipping: $component not built"
    exit 0
fi
if [[ ! -x "$runner" ]]; then
    echo "skipping: $runner not built"
    exit 0
fi
if [[ ! -x "$wrapper" ]]; then
    echo "FAIL: $wrapper not executable"
    exit 1
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

passed=0
failed=0

# Run wrapper, capture stdout. On failure print the actual vs.
# expected and increment the fail counter.
assert_contains() {
    local label="$1"
    local needle="$2"
    local actual="$3"
    if [[ "$actual" == *"$needle"* ]]; then
        passed=$((passed + 1))
        echo "PASS: $label"
    else
        failed=$((failed + 1))
        echo "FAIL: $label"
        echo "  expected substring: $needle"
        echo "  actual: $actual"
    fi
}

# Shape 1: bare interactive with `.quit` piped in.
out="$(printf '.quit\n' | "$wrapper" 2>&1 || true)"
assert_contains "bare interactive prints prompt" "sqlite>" "$out"

# Shape 2: :memory: with inline SQL via stdin.
out="$(printf 'SELECT 1+1;\n.quit\n' | "$wrapper" :memory: 2>&1 || true)"
assert_contains ":memory: SELECT 1+1 -> 2" "2" "$out"

# Shape 3: file-backed db that round-trips across two invocations.
db="$tmpdir/round.db"
"$wrapper" "$db" "CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (42);" >/dev/null 2>&1 || true
out="$(printf 'SELECT * FROM t;\n.quit\n' | "$wrapper" "$db" 2>&1 || true)"
assert_contains "file-backed DB persists across sessions" "42" "$out"

# Shape 4: one-shot DB SQL form.
out="$("$wrapper" :memory: "SELECT 99 AS canary;" 2>&1 || true)"
assert_contains "one-shot DB SQL canary" "99" "$out"

# Shape 5: multi-statement stdin pipe.
out="$(printf 'SELECT 7;\nSELECT 8;\nSELECT 9;\n.quit\n' | "$wrapper" :memory: 2>&1 || true)"
for n in 7 8 9; do
    assert_contains "multi-statement stdin: $n" "$n" "$out"
done

# Shape 6: dot-command from stdin.
out="$(printf '.version\n.quit\n' | "$wrapper" :memory: 2>&1 || true)"
assert_contains ".version reports SQLite version" "SQLite 3." "$out"

# Shape 7: -- passthrough -- args after `--` reach the cli's argv.
# Currently the cli ignores extra positional args after the db, but
# it shouldn't ERROR on them. We just check the SQL still runs.
out="$(printf 'SELECT 11;\n.quit\n' | "$wrapper" :memory: -- extra-arg 2>&1 || true)"
assert_contains "-- passthrough doesn't break SQL" "11" "$out"

echo
echo "Total: $((passed + failed)) ($passed passed, $failed failed)"
if (( failed > 0 )); then
    exit 1
fi
