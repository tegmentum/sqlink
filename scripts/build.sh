#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="${PROJECT_DIR}/build"
BUILD_TYPE="${BUILD_TYPE:-Release}"

# Find WASI SDK
if [[ -z "${WASI_SDK_PREFIX}" ]]; then
    for loc in "$HOME/wasi-sdk-33" "$HOME/wasi-sdk" "/opt/wasi-sdk"; do
        [[ -d "$loc" ]] && export WASI_SDK_PREFIX="$loc" && break
    done
fi

[[ -z "${WASI_SDK_PREFIX}" ]] && echo "Error: WASI SDK not found" && exit 1

JOBS=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)

echo "Building sqlink..."
echo "WASI SDK: ${WASI_SDK_PREFIX}"

[[ ! -f "${PROJECT_DIR}/deps/sqlite/sqlite3.c" ]] && "${SCRIPT_DIR}/download-sqlite.sh"

[[ "$1" == "--clean" ]] && rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"

cmake -B "$BUILD_DIR" -S "$PROJECT_DIR" \
    -DCMAKE_TOOLCHAIN_FILE="${PROJECT_DIR}/toolchain/wasi-sdk-p2.cmake" \
    -DCMAKE_BUILD_TYPE="${BUILD_TYPE}"

cmake --build "$BUILD_DIR" --parallel "$JOBS"

echo "Build complete: $BUILD_DIR/bin/sqlite.wasm"
