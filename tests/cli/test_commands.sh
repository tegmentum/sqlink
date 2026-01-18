#!/bin/bash
# Test new CLI commands for SQLite WASM
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
BUILD_DIR="${PROJECT_ROOT}/build"
CLI_WASM="${BUILD_DIR}/sqlite-cli.wasm"
TMP_DIR="/tmp/sqlite-cli-test-$$"

# Check if wasmtime is available
if ! command -v wasmtime &> /dev/null; then
    echo "SKIP: wasmtime not found"
    exit 0
fi

# Check if CLI is built
if [ ! -f "$CLI_WASM" ]; then
    echo "SKIP: CLI not built (run 'make cli' first)"
    exit 0
fi

# Create temp directory
mkdir -p "$TMP_DIR"
trap "rm -rf $TMP_DIR" EXIT

echo "Testing SQLite CLI commands..."

# Helper function to run CLI command
run_cli() {
    wasmtime run --dir="$TMP_DIR" --dir=. "$CLI_WASM" -- "$@" 2>&1 || true
}

# ============================================================================
# Phase 1: .read and .output tests
# ============================================================================

echo ""
echo "=== Phase 1: .read and .output ==="

# Test .read command
echo "Test: .read command..."
cat > "$TMP_DIR/test.sql" << 'EOF'
CREATE TABLE test_read(id INTEGER, name TEXT);
INSERT INTO test_read VALUES(1, 'Alice');
INSERT INTO test_read VALUES(2, 'Bob');
SELECT * FROM test_read;
EOF

OUTPUT=$(run_cli "$TMP_DIR/test.db" ".read $TMP_DIR/test.sql")
if echo "$OUTPUT" | grep -q "Alice" && echo "$OUTPUT" | grep -q "Bob"; then
    echo "  PASS: .read executes SQL from file"
else
    echo "  FAIL: .read did not execute SQL correctly"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .output command
echo "Test: .output command..."
OUTPUT=$(run_cli "$TMP_DIR/test.db" ".output $TMP_DIR/out.txt" "SELECT * FROM test_read;" ".output stdout")
if [ -f "$TMP_DIR/out.txt" ] && grep -q "Alice" "$TMP_DIR/out.txt"; then
    echo "  PASS: .output redirects to file"
else
    echo "  FAIL: .output did not create file or content missing"
    exit 1
fi

# ============================================================================
# Phase 2: Data management commands
# ============================================================================

echo ""
echo "=== Phase 2: Data management ==="

# Test .backup command
echo "Test: .backup command..."
OUTPUT=$(run_cli "$TMP_DIR/test.db" ".backup $TMP_DIR/backup.db")
if [ -f "$TMP_DIR/backup.db" ]; then
    echo "  PASS: .backup creates backup file"
else
    echo "  FAIL: .backup did not create backup file"
    exit 1
fi

# Test .restore command
echo "Test: .restore command..."
OUTPUT=$(run_cli "$TMP_DIR/restored.db" ".restore $TMP_DIR/backup.db" "SELECT * FROM test_read;")
if echo "$OUTPUT" | grep -q "Alice"; then
    echo "  PASS: .restore restores database"
else
    echo "  FAIL: .restore did not restore correctly"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .clone command
echo "Test: .clone command..."
OUTPUT=$(run_cli "$TMP_DIR/test.db" ".clone $TMP_DIR/cloned.db")
if [ -f "$TMP_DIR/cloned.db" ]; then
    echo "  PASS: .clone creates cloned database"
else
    echo "  FAIL: .clone did not create cloned file"
    exit 1
fi

# Test .import command with CSV
echo "Test: .import command..."
cat > "$TMP_DIR/data.csv" << 'EOF'
name,age,city
Carol,30,NYC
Dave,25,LA
EOF

OUTPUT=$(run_cli "$TMP_DIR/import.db" ".import $TMP_DIR/data.csv people" "SELECT * FROM people;")
if echo "$OUTPUT" | grep -q "Carol" && echo "$OUTPUT" | grep -q "Dave"; then
    echo "  PASS: .import imports CSV data"
else
    echo "  FAIL: .import did not import correctly"
    echo "  Got: $OUTPUT"
    exit 1
fi

# ============================================================================
# Phase 3: Query analysis commands
# ============================================================================

echo ""
echo "=== Phase 3: Query analysis ==="

# Test .changes command
echo "Test: .changes command..."
OUTPUT=$(run_cli :memory: "CREATE TABLE t(x);" "INSERT INTO t VALUES(1),(2),(3);" ".changes")
if echo "$OUTPUT" | grep -q "Changes: 3"; then
    echo "  PASS: .changes shows row count"
else
    echo "  FAIL: .changes output incorrect"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .timer command
echo "Test: .timer command..."
OUTPUT=$(run_cli :memory: ".timer on" "SELECT 1;" ".timer off")
if echo "$OUTPUT" | grep -q "Run Time"; then
    echo "  PASS: .timer shows execution time"
else
    echo "  FAIL: .timer did not show time"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .timeout command
echo "Test: .timeout command..."
OUTPUT=$(run_cli :memory: ".timeout 5000")
if echo "$OUTPUT" | grep -q "Timeout set to 5000"; then
    echo "  PASS: .timeout sets timeout"
else
    echo "  FAIL: .timeout did not set correctly"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .trace command
echo "Test: .trace command..."
OUTPUT=$(run_cli :memory: ".trace on" "SELECT 1;" ".trace off")
if echo "$OUTPUT" | grep -q "TRACE:"; then
    echo "  PASS: .trace shows SQL"
else
    echo "  FAIL: .trace did not show SQL"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .eqp command
echo "Test: .eqp command..."
OUTPUT=$(run_cli :memory: "CREATE TABLE t(x);" ".eqp on" "SELECT * FROM t;")
if echo "$OUTPUT" | grep -q "QUERY PLAN"; then
    echo "  PASS: .eqp shows query plan"
else
    echo "  FAIL: .eqp did not show plan"
    echo "  Got: $OUTPUT"
    exit 1
fi

# ============================================================================
# Phase 4: Database information commands
# ============================================================================

echo ""
echo "=== Phase 4: Database information ==="

# Test .dbinfo command
echo "Test: .dbinfo command..."
OUTPUT=$(run_cli :memory: ".dbinfo")
if echo "$OUTPUT" | grep -q "page_size" && echo "$OUTPUT" | grep -q "encoding"; then
    echo "  PASS: .dbinfo shows database info"
else
    echo "  FAIL: .dbinfo output incomplete"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .fullschema command
echo "Test: .fullschema command..."
OUTPUT=$(run_cli :memory: "CREATE TABLE t(x);" "CREATE INDEX idx ON t(x);" ".fullschema")
if echo "$OUTPUT" | grep -q "CREATE TABLE" && echo "$OUTPUT" | grep -q "CREATE INDEX"; then
    echo "  PASS: .fullschema shows complete schema"
else
    echo "  FAIL: .fullschema output incomplete"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .limit command
echo "Test: .limit command..."
OUTPUT=$(run_cli :memory: ".limit")
if echo "$OUTPUT" | grep -q "SQLITE_LIMIT_LENGTH"; then
    echo "  PASS: .limit shows limits"
else
    echo "  FAIL: .limit output incorrect"
    echo "  Got: $OUTPUT"
    exit 1
fi

# ============================================================================
# Phase 5: Additional commands
# ============================================================================

echo ""
echo "=== Phase 5: Additional commands ==="

# Test .once command
echo "Test: .once command..."
OUTPUT=$(run_cli :memory: ".once $TMP_DIR/once.txt" "SELECT 'hello';" "SELECT 'world';")
if [ -f "$TMP_DIR/once.txt" ] && grep -q "hello" "$TMP_DIR/once.txt" && ! grep -q "world" "$TMP_DIR/once.txt"; then
    echo "  PASS: .once outputs to file once"
else
    echo "  FAIL: .once did not work correctly"
    exit 1
fi

# Test .vfslist command
echo "Test: .vfslist command..."
OUTPUT=$(run_cli :memory: ".vfslist")
if echo "$OUTPUT" | grep -q "VFS Name"; then
    echo "  PASS: .vfslist shows VFS list"
else
    echo "  FAIL: .vfslist output incorrect"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .vfsname command
echo "Test: .vfsname command..."
OUTPUT=$(run_cli :memory: ".vfsname")
if [ -n "$OUTPUT" ]; then
    echo "  PASS: .vfsname shows VFS name"
else
    echo "  FAIL: .vfsname returned empty"
    exit 1
fi

# Test .show command (updated)
echo "Test: .show command (updated)..."
OUTPUT=$(run_cli :memory: ".show")
if echo "$OUTPUT" | grep -q "timer:" && echo "$OUTPUT" | grep -q "trace:" && echo "$OUTPUT" | grep -q "output:"; then
    echo "  PASS: .show includes new settings"
else
    echo "  FAIL: .show missing new settings"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test .help command (updated)
echo "Test: .help command (updated)..."
OUTPUT=$(run_cli :memory: ".help")
EXPECTED_CMDS=(".backup" ".changes" ".clone" ".dbinfo" ".eqp" ".fullschema" ".import" ".limit" ".once" ".restore" ".save" ".timeout" ".timer" ".trace" ".vfslist" ".vfsname")
MISSING=""
for cmd in "${EXPECTED_CMDS[@]}"; do
    if ! echo "$OUTPUT" | grep -q "$cmd"; then
        MISSING="$MISSING $cmd"
    fi
done
if [ -z "$MISSING" ]; then
    echo "  PASS: .help includes all new commands"
else
    echo "  FAIL: .help missing:$MISSING"
    exit 1
fi

echo ""
echo "========================================"
echo "All CLI command tests passed!"
echo "========================================"
