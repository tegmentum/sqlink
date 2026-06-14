# Plan: Port well-known SQLite plugins to our component system

## Goal

Ship wasm-component implementations of the well-known SQLite
extensions (json1, fts5, rtree, etc.) targeting our canonical
`sqlite:extension/{minimal,stateful,full}` worlds so users get
the SQLite extension ecosystem they expect via `.load`.

## Current state

`extensions/{json1,fts5,geopoly,rtree}/` contains scaffolds with
a prior obsolete WIT world (`json1-extension`, etc.) that
predates the canonical extension architecture. They don't
compile against the current host world and need rewriting, not
renaming.

`~/git/sqlite-wasm-loader/runtimes/wasmtime/` has demo Rust
extensions (`agg-extension`, `crypto-extension`, `math-extension`,
`uuid-extension`, etc.) that DO target the canonical world.
Those are the right structural reference, but they're demos —
not ports of the well-known SQLite extensions.

## Three tiers by porting effort

### Tier 1 — pure scalar functions (days each)

Library of independent SQL functions, no virtual tables, no
aggregates, no state across calls. Easiest port shape.

- **json1** — `json()`, `json_extract()`, `json_array()`,
  `json_object()`, `json_patch()`, `json_remove()`, etc.
- **regexp** — `regexp_like()`, `regexp_substr()`,
  `regexp_replace()`, POSIX or PCRE flavor
- **uuid** — `uuid()`, `uuidv4()`, `uuidv7()` (already have a
  demo to promote)
- **crypto** — `sha1()`, `sha256()`, `md5()`, `hex()`, `base64()`
- **math** — `pow()`, `floor()`, `ceil()`, `sqrt()`, `log()`,
  `trig()` (sin/cos/tan), `degrees()`, `radians()`
- **series** — `generate_series()` (borderline tier 2: this is
  a table-valued function; needs the tier-3 vtab dispatch to
  land first)

### Tier 2 — aggregate / window functions (~few days each)

- **stats** — `stddev()`, `variance()`, `median()`,
  `percentile()`, `mode()`
- **closure** — recursive graph closures
- **decimal** — fixed-point decimal arithmetic aggregates

Phase 1's window-function dispatch (xValue + xInverse) is the
prerequisite for window-mode use; that work shipped already
(commit 97c2c43).

### Tier 3 — virtual tables (weeks)

- **fts5** — full-text search
- **rtree** — spatial / range index
- **geopoly** — polygon spatial
- **csv** — CSV virtual table
- **dbstat** — page-level diagnostics

We don't have virtual-table dispatch yet. The canonical
`sqlite:extension` world covers scalar / aggregate / collation /
hooks / authorizer but **not vtab**. Implementing vtab is a real
prerequisite for this entire tier.

## Decisions locked in

| | |
|---|---|
| First port | **json1**  Highest-impact scalar functions; SQLite's own json1 is the model; clean tier-1 exercise that validates the architecture end-to-end without surprises. |
| Vtab plan | **Commit to vtab dispatch as Tier 3 prereq.** Required for FTS5 / RTree / Geopoly. ~1 week of focused dispatch work, then per-extension ports. |

## Per-extension shape (Tier 1)

Each extension lives at `extensions/<name>/` with:

```
extensions/json1/
├── Cargo.toml          # cdylib targeting wasm32-wasip2
├── wit/world.wit       # imports sqlite:extension/{minimal,types,policy}
├── wit/deps/           # vendored sqlite-extension WIT (symlink or copy)
├── src/lib.rs          # impl Guest for ScalarFunction + Metadata
└── tests/integration.rs # native test driving the host's dispatch
```

The Rust source implements:
- `metadata.describe()` returning the manifest (function specs + capability declarations)
- `scalar_function.call(func_id, args)` matching on func_id to
  dispatch to the right implementation

For json1 specifically, we use the Rust `serde_json` crate
(pure-Rust, no_std-friendly with `alloc` feature, builds wasm32-
clean) so we don't have to port SQLite's C json1 source.

For regexp, the `regex` crate (or `regex-lite` for code size).
For crypto, the `sha2` + `md5` + `hex` + `base64` crates.

Strategy: prefer pure-Rust dep ports over C source. C source
porting (where it would be necessary — e.g. fts5) goes through
wasi-sdk and a glue layer; that's tier 3 territory.

## Vtab dispatch (tier 3 prereq)

The work this requires, mirroring the patterns we landed for
scalar / aggregate / collation / hook:

1. **WIT extension** — add a `vtab` interface to
   `sqlite-loader-wit/wit/guest.wit` covering xCreate, xConnect,
   xBestIndex, xOpen, xFilter, xNext, xEof, xColumn, xRowid,
   xUpdate, xDestroy
2. **Bindgen + Host trait** — extend `host/src/lib.rs` with
   `loaded_vtab` bindgen against a new `vtab` world; per-method
   dispatch (`dispatch_vtab_filter`, etc.)
3. **Connection wiring** — `cli/src/lib.rs`'s `do_load` adds an
   `if manifest.has_vtabs` branch that calls
   `sqlite3_create_module_v2` and routes the per-method
   callbacks through `dispatch::vtab_*`
4. **Reference port** — `csv` first (simplest vtab in the
   SQLite ecosystem) to validate the dispatch path; then fts5,
   rtree, geopoly.

## Order of operations

1. **json1 port** (tier 1 reference; ~3 days)
2. **regexp + crypto + math + uuid** (3 more tier 1 ports; ~1
   week, can parallelize)
3. **stats** (tier 2 reference; ~3 days)
4. **vtab dispatch** (tier 3 prereq; ~1 week)
5. **csv port** (tier 3 reference; ~3 days)
6. **fts5** (tier 3 headline; ~1 week)
7. **rtree + geopoly** (tier 3; ~3 days each)

Total ~4-5 weeks for a substantial extension catalog.

## Cli integration

- `.load https://extensions.tegmentum.dev/json1.wasm` — fetches
  via CAS cache (Plan 1), loads, registers the json1 functions
- `.extensions` — lists currently loaded extensions
- All existing `.load` machinery (capability gates, signature
  verification per `TrustPolicy::Ed25519Signed`, dispatch
  routing) applies uniformly

## Validation

Each ported extension ships:
- Unit tests for the function implementations (pure Rust)
- Integration test: build the wasm, load it via the cli, run a
  query that uses the function, assert result matches SQLite's
  own behavior on the same query

## Open questions

None for tier 1. Tier 3 has architectural questions deferred to
that phase (vtab transaction semantics across the host
boundary, etc.).
