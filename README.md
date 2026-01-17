# sqlite-wasm

SQLite database engine compiled to WebAssembly Component Model targeting WASI Preview 2.

> **Note**: This repository contains the pure SQLite WebAssembly component. For the extension that embeds Wasmtime and manages WASM extensions within SQLite, see [sqlite-webassembly-extension](https://github.com/anthropics/sqlite-webassembly-extension).

## Features

- Full SQL support
- In-memory and file-based databases
- FTS5 full-text search
- R-Tree spatial indexing
- JSON1 extension
- GeoPoly extension (for GeoPackage)
- Thread-safe disabled (single-threaded WASI)

## Building

```bash
./scripts/download-sqlite.sh
./scripts/build.sh
```

## WIT Interface

```wit
interface database {
    resource connection {
        open: static func(path: string) -> result<connection, result-code>;
        open-memory: static func() -> result<connection, result-code>;
        exec: func(sql: string) -> result<_, result-code>;
        prepare: func(sql: string) -> result<statement, result-code>;
    }

    resource statement {
        bind-int: func(index: s32, value: s64) -> result-code;
        bind-text: func(index: s32, value: string) -> result-code;
        step: func() -> result-code;
        column-text: func(index: s32) -> string;
    }
}
```

## Usage

```c
// Open database
sqlite3* db;
sqlite3_open(":memory:", &db);

// Execute SQL
sqlite3_exec(db, "CREATE TABLE test (id INTEGER, name TEXT)", NULL, NULL, NULL);

// Prepared statements
sqlite3_stmt* stmt;
sqlite3_prepare_v2(db, "INSERT INTO test VALUES (?, ?)", -1, &stmt, NULL);
sqlite3_bind_int(stmt, 1, 42);
sqlite3_bind_text(stmt, 2, "hello", -1, SQLITE_STATIC);
sqlite3_step(stmt);
sqlite3_finalize(stmt);

sqlite3_close(db);
```

## Size

Approximate component size: ~1.2 MB

## License

SQLite is in the public domain.
