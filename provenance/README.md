# Plugin provenance

`extensions.db` tracks the "what + where + who + when" of every
loadable extension we ship.

## Layout

- `schema.sql`  table + view definitions (DDL only)
- `scan.py`  scanner that walks `extensions/*` and ingests
  Cargo.toml metadata, source content hash, built wasm
  artifacts, declared SQL surface, and declared capabilities
- `extensions.db`  the populated DB; checked in for review
  diffs and offline querying

## Re-running

```sh
python3 provenance/scan.py
```

The scanner is idempotent: a (plugin, version, source_sha256)
that already exists is upserted, not duplicated. Re-run after
adding a new extension, after rebuilding artifacts, or after
editing source.

## Schema overview

| Table | Rows | What |
|---|---:|---|
| `plugin` | 40 | one row per `extensions/<name>/` dir |
| `plugin_version` | 40 | (plugin, version, source_sha256) tuples |
| `dependency` | ~200 | direct deps from each Cargo.toml |
| `artifact` | 63 | built .wasm and .component.wasm files |
| `sql_function` | 644 | scalar / aggregate / vtab functions |
| `capability` | 1 | declared capabilities (Http only so far) |

## Useful queries

```sql
-- Catalog snapshot.
SELECT * FROM plugin_latest ORDER BY plugin;

-- Which crates we depend on the most.
SELECT name, count(*) AS used_in FROM dependency
  WHERE source = 'crates.io'
  GROUP BY name ORDER BY used_in DESC LIMIT 20;

-- SQL surface a particular plugin exposes.
SELECT * FROM sql_surface WHERE plugin = 'vec0';

-- Artifact sizes for review.
SELECT plugin, artifact_kind, size_bytes
FROM artifact_index ORDER BY size_bytes DESC;

-- Plugins by world.
SELECT declared_world, count(*) FROM plugin
  WHERE declared_world IS NOT NULL
  GROUP BY declared_world;

-- License inventory.
SELECT license, count(*) FROM plugin_version
  GROUP BY license ORDER BY count(*) DESC;
```

## C-bundled vs Rust plugins

The catalog covers two kinds:
- **rust** (36 plugins)  Cargo-built wasm components.
- **c-bundled** (4 plugins: fts5, rtree, geopoly, wasm-demo)
   compiled into the bundled SQLite via `LIBSQLITE3_FLAGS`.
   No Rust Cargo manifest; the scanner records a minimal
  row keyed by the C source-file hash.

## What's NOT tracked yet

- Transitive deps (only direct `[dependencies]` from each
  Cargo.toml is captured). Pull these from Cargo.lock if
  needed.
- Per-function `FunctionFlags` (deterministic etc.)  the
  static grep doesn't parse the surrounding flag enum yet.
- Test counts and per-function coverage  out of scope; the
  test suite is the source of truth.
- The cli's bundled flag-based extensions (dbstat, bytecode,
  sqlite_stmt, session)  no source dir of their own.

## Schema version

The `schema_version` table records the DDL revision.  Current
version: 1.  Bump when adding columns and write an explicit
ALTER migration in scan.py.
