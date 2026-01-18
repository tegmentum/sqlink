#!/bin/bash
# Test extension loading commands in the SQLite CLI
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
BUILD_DIR="${PROJECT_ROOT}/build"
CLI_WASM="${BUILD_DIR}/sqlite-cli.wasm"
EXT_DIR="${BUILD_DIR}/extensions"

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

echo "Testing SQLite CLI extension loading commands..."

# Test 1: .extensions command (no extensions loaded)
echo "Test 1: .extensions with no extensions..."
OUTPUT=$(echo ".extensions" | wasmtime run --dir=. "$CLI_WASM" -- :memory: 2>&1 || true)
if echo "$OUTPUT" | grep -q "No extensions loaded"; then
    echo "  PASS: .extensions shows no extensions"
else
    echo "  FAIL: Expected 'No extensions loaded'"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test 2: .load command without argument
echo "Test 2: .load without argument..."
OUTPUT=$(echo ".load" | wasmtime run --dir=. "$CLI_WASM" -- :memory: 2>&1 || true)
if echo "$OUTPUT" | grep -q "Usage: .load FILENAME"; then
    echo "  PASS: .load shows usage"
else
    echo "  FAIL: Expected usage message"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test 3: .unload command without argument
echo "Test 3: .unload without argument..."
OUTPUT=$(echo ".unload" | wasmtime run --dir=. "$CLI_WASM" -- :memory: 2>&1 || true)
if echo "$OUTPUT" | grep -q "Usage: .unload NAME"; then
    echo "  PASS: .unload shows usage"
else
    echo "  FAIL: Expected usage message"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test 4: .load with a path (extension loading simulation)
echo "Test 4: .load with extension path..."
OUTPUT=$(echo ".load test_extension.wasm" | wasmtime run --dir=. "$CLI_WASM" -- :memory: 2>&1 || true)
if echo "$OUTPUT" | grep -q "Loaded extension: test_extension"; then
    echo "  PASS: .load reports extension loaded"
else
    echo "  FAIL: Expected 'Loaded extension: test_extension'"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test 5: .extensions after loading
echo "Test 5: .extensions after loading..."
OUTPUT=$(printf ".load test_extension.wasm\n.extensions\n" | wasmtime run --dir=. "$CLI_WASM" -- :memory: 2>&1 || true)
if echo "$OUTPUT" | grep -q "test_extension"; then
    echo "  PASS: .extensions shows loaded extension"
else
    echo "  FAIL: Expected extension in list"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test 6: .unload after loading
echo "Test 6: .unload after loading..."
OUTPUT=$(printf ".load test_extension.wasm\n.unload test_extension\n.extensions\n" | wasmtime run --dir=. "$CLI_WASM" -- :memory: 2>&1 || true)
if echo "$OUTPUT" | grep -q "Unloaded extension: test_extension"; then
    echo "  PASS: .unload reports extension unloaded"
else
    echo "  FAIL: Expected 'Unloaded extension'"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test 7: .unload non-existent extension
echo "Test 7: .unload non-existent extension..."
OUTPUT=$(echo ".unload nonexistent" | wasmtime run --dir=. "$CLI_WASM" -- :memory: 2>&1 || true)
if echo "$OUTPUT" | grep -q "not found"; then
    echo "  PASS: .unload reports extension not found"
else
    echo "  FAIL: Expected 'not found' message"
    echo "  Got: $OUTPUT"
    exit 1
fi

# Test 8: .help includes extension commands
echo "Test 8: .help includes extension commands..."
OUTPUT=$(echo ".help" | wasmtime run --dir=. "$CLI_WASM" -- :memory: 2>&1 || true)
if echo "$OUTPUT" | grep -q ".load FILE" && echo "$OUTPUT" | grep -q ".unload NAME" && echo "$OUTPUT" | grep -q ".extensions"; then
    echo "  PASS: .help includes extension commands"
else
    echo "  FAIL: Expected extension commands in help"
    echo "  Got: $OUTPUT"
    exit 1
fi

echo ""
echo "All CLI extension loading tests passed!"
