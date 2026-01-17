#!/bin/bash
# Generate C bindings from WIT definitions
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
WIT_DIR="${PROJECT_ROOT}/wit"
BINDINGS_DIR="${PROJECT_ROOT}/src/bindings"

echo "Generating C bindings from WIT..."

# Check for wit-bindgen
if ! command -v wit-bindgen &> /dev/null; then
    echo "Error: wit-bindgen not found"
    echo "Install with: cargo install wit-bindgen-cli"
    exit 1
fi

# Create bindings directory
mkdir -p "$BINDINGS_DIR"

# Generate bindings
wit-bindgen c "$WIT_DIR" \
    --world sqlite-world \
    --out-dir "$BINDINGS_DIR"

echo "Bindings generated in ${BINDINGS_DIR}"

# List generated files
if [ -d "$BINDINGS_DIR" ]; then
    echo "Generated files:"
    ls -la "$BINDINGS_DIR"
fi
