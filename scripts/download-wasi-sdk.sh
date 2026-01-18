#!/bin/bash
# Download wasi-sdk toolchain
set -euo pipefail

WASI_SDK_VERSION="${WASI_SDK_VERSION:-29}"
WASI_SDK_MAJOR="${WASI_SDK_VERSION}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
DEPS_DIR="${PROJECT_ROOT}/deps"

# Detect platform and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
    darwin)
        case "$ARCH" in
            arm64)
                PLATFORM="arm64-macos"
                ;;
            x86_64)
                PLATFORM="x86_64-macos"
                ;;
            *)
                echo "Unsupported architecture: $ARCH"
                exit 1
                ;;
        esac
        ;;
    linux)
        case "$ARCH" in
            x86_64)
                PLATFORM="x86_64-linux"
                ;;
            aarch64)
                PLATFORM="aarch64-linux"
                ;;
            *)
                echo "Unsupported architecture: $ARCH"
                exit 1
                ;;
        esac
        ;;
    *)
        echo "Unsupported OS: $OS"
        exit 1
        ;;
esac

WASI_SDK_URL="https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-${WASI_SDK_MAJOR}/wasi-sdk-${WASI_SDK_MAJOR}.0-${PLATFORM}.tar.gz"
WASI_SDK_DIR="${DEPS_DIR}/wasi-sdk-${WASI_SDK_MAJOR}.0-${PLATFORM}"

echo "Downloading wasi-sdk ${WASI_SDK_VERSION} for ${PLATFORM}..."
echo "URL: ${WASI_SDK_URL}"

# Create temporary directory
TMP_DIR=$(mktemp -d)
trap "rm -rf $TMP_DIR" EXIT

# Download and extract
cd "$TMP_DIR"
curl -fsSL -o wasi-sdk.tar.gz "$WASI_SDK_URL"
tar -xzf wasi-sdk.tar.gz

# Move to deps directory
EXTRACTED_DIR=$(ls -d wasi-sdk-* 2>/dev/null | head -1)
if [ -z "$EXTRACTED_DIR" ]; then
    echo "Error: Could not find extracted wasi-sdk directory"
    exit 1
fi

# Remove old installation if exists
rm -rf "${DEPS_DIR}/wasi-sdk"
rm -rf "${WASI_SDK_DIR}"

# Move to deps
mv "$EXTRACTED_DIR" "${DEPS_DIR}/"

# Create symlink for easier access
cd "${DEPS_DIR}"
ln -sf "$(basename "${WASI_SDK_DIR}")" wasi-sdk

echo "wasi-sdk ${WASI_SDK_VERSION} installed to ${DEPS_DIR}/wasi-sdk"

# Verify installation
if [ -x "${DEPS_DIR}/wasi-sdk/bin/clang" ]; then
    echo "Verification passed: clang binary found"
    "${DEPS_DIR}/wasi-sdk/bin/clang" --version
else
    echo "Error: clang binary not found in wasi-sdk"
    exit 1
fi
