#!/bin/bash
# Test script for sqlite-wasm-ext CLI

set -e

CLI="./cli/sqlite-wasm-ext"
TEST_DIR="/tmp/sqlite-wasm-cli-test"
REGISTRY_FILE="$(pwd)/registry/index.json"

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m' # No Color

passed=0
failed=0

pass() {
    echo -e "  ${GREEN}✓${NC} $1"
    passed=$((passed + 1))
}

fail() {
    echo -e "  ${RED}✗${NC} $1"
    failed=$((failed + 1))
}

# Clean up test directory
rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

echo "SQLite WASM Extension CLI Tests"
echo "================================"
echo ""

# Test 1: Help command
echo "1. Testing help command"
if $CLI --help | grep -q "SQLite WASM Extension Manager"; then
    pass "Help command works"
else
    fail "Help command failed"
fi

# Test 2: Sync with local registry
echo ""
echo "2. Testing sync command"
if $CLI -d "$TEST_DIR" sync --url "file://$REGISTRY_FILE" | grep -q "Synced"; then
    pass "Sync with local registry works"
else
    fail "Sync failed"
fi

# Test 3: Search functionality
echo ""
echo "3. Testing search command"
if $CLI -d "$TEST_DIR" search text | grep -q "text"; then
    pass "Search for 'text' works"
else
    fail "Search for 'text' failed"
fi

if $CLI -d "$TEST_DIR" search hash | grep -q "crypto"; then
    pass "Search by keyword 'hash' works"
else
    fail "Search by keyword failed"
fi

if $CLI -d "$TEST_DIR" search nonexistent12345 | grep -q "No extensions found"; then
    pass "Search for nonexistent returns correct message"
else
    fail "Search for nonexistent failed"
fi

# Test 4: Info command
echo ""
echo "4. Testing info command"
if $CLI -d "$TEST_DIR" info text | grep -q "Extension: text"; then
    pass "Info command shows extension name"
else
    fail "Info command failed to show name"
fi

if $CLI -d "$TEST_DIR" info text | grep -q "Exports"; then
    pass "Info command shows exports"
else
    fail "Info command failed to show exports"
fi

if $CLI -d "$TEST_DIR" info text | grep -q "Not installed"; then
    pass "Info shows not installed status"
else
    fail "Info failed to show installation status"
fi

# Test 5: List command
echo ""
echo "5. Testing list command"
if $CLI -d "$TEST_DIR" list --installed | grep -q "No extensions installed"; then
    pass "List installed shows none initially"
else
    fail "List installed failed"
fi

if $CLI -d "$TEST_DIR" list --available | grep -q "text"; then
    pass "List available shows extensions"
else
    fail "List available failed"
fi

# Test 6: Install error handling (no OCI artifact)
echo ""
echo "6. Testing install error handling"
if $CLI -d "$TEST_DIR" install text 2>&1 | grep -q "no OCI artifact"; then
    pass "Install correctly reports missing OCI artifact"
else
    fail "Install error handling for missing artifact failed"
fi

# Test 7: Uninstall error handling
echo ""
echo "7. Testing uninstall error handling"
if $CLI -d "$TEST_DIR" uninstall text 2>&1 | grep -q "is not installed"; then
    pass "Uninstall correctly reports not installed"
else
    fail "Uninstall error handling failed"
fi

# Test 8: Simulated install/uninstall flow
echo ""
echo "8. Testing simulated install/uninstall flow"

# Manually create an installed extension
EXT_DIR="$TEST_DIR/extensions/text"
mkdir -p "$EXT_DIR"
cat > "$EXT_DIR/extension.json" << 'EOF'
{"name": "text", "version": "0.1.0"}
EOF

# Insert into database
sqlite3 "$TEST_DIR/registry.db" << EOF
INSERT OR REPLACE INTO installed (name, version, installed_at, path, checksum, oci_artifact)
VALUES ('text', '0.1.0', datetime('now'), '$EXT_DIR', 'sha256:test', 'ghcr.io/test/text:0.1.0');
EOF

if $CLI -d "$TEST_DIR" list --installed | grep -q "text"; then
    pass "Simulated install shows in list"
else
    fail "Simulated install not showing"
fi

if $CLI -d "$TEST_DIR" info text | grep -q "Installed:"; then
    pass "Info shows installed status"
else
    fail "Info not showing installed status"
fi

# Test uninstall
if $CLI -d "$TEST_DIR" uninstall text | grep -q "Successfully uninstalled"; then
    pass "Uninstall command succeeds"
else
    fail "Uninstall command failed"
fi

if [ ! -d "$EXT_DIR" ]; then
    pass "Uninstall removes extension directory"
else
    fail "Uninstall did not remove directory"
fi

if $CLI -d "$TEST_DIR" list --installed | grep -q "No extensions installed"; then
    pass "List shows no extensions after uninstall"
else
    fail "List still shows extension after uninstall"
fi

# Test 9: Update command (with nothing to update)
echo ""
echo "9. Testing update command"
if $CLI -d "$TEST_DIR" update 2>&1 | grep -q "up to date\|Synced"; then
    pass "Update command works"
else
    fail "Update command failed"
fi

# Test 10: Publish error handling (no token)
echo ""
echo "10. Testing publish error handling"
if $CLI -d "$TEST_DIR" publish ./registry/extension-manager text 0.1.0 2>&1 | grep -q "No authentication token"; then
    pass "Publish correctly reports missing token"
else
    fail "Publish error handling failed"
fi

# Test 11: OCI reference parsing
echo ""
echo "11. Testing extension-manager info (with OCI artifact)"
if $CLI -d "$TEST_DIR" info extension-manager | grep -q "ghcr.io"; then
    pass "Info shows OCI artifact"
else
    fail "Info not showing OCI artifact"
fi

# Summary
echo ""
echo "================================"
total=$((passed + failed))
echo "Tests: $total total, $passed passed, $failed failed"
echo "================================"

# Clean up
rm -rf "$TEST_DIR"

if [ $failed -gt 0 ]; then
    exit 1
fi
