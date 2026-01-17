#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPS_DIR="$(cd "$SCRIPT_DIR/../deps" && pwd)"

SQLITE_VERSION="3450100"
SQLITE_YEAR="2024"
SQLITE_URL="https://sqlite.org/${SQLITE_YEAR}/sqlite-amalgamation-${SQLITE_VERSION}.zip"

mkdir -p "$DEPS_DIR"
cd "$DEPS_DIR"

if [[ -f "sqlite/sqlite3.c" ]]; then
    echo "SQLite already downloaded"
    exit 0
fi

echo "Downloading SQLite amalgamation..."
curl -L -o "sqlite-amalgamation.zip" "$SQLITE_URL"

echo "Extracting..."
unzip -q "sqlite-amalgamation.zip"
mv "sqlite-amalgamation-${SQLITE_VERSION}" sqlite
rm "sqlite-amalgamation.zip"

echo "SQLite downloaded to ${DEPS_DIR}/sqlite"
