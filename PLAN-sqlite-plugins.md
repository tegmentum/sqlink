# Plan: Port well-known SQLite plugins to our component system

> **Status (2026-06-16): shipped.** 513 SQL-callable functions
> (scalars + aggregates) plus 11 virtual-table modules (csv,
> fts5, rtree, geopoly, raster_polygon_dump, dbstat,
> sqlite_stmt, bytecode, generate_series, vec0, vec_each,
> listargs) all delivered  see "Final state (delivered)" below
> for the per-source breakdown. vec0 ships five pluggable
> backends (brute / IVF / HNSW / int8-HNSW / binary LSH) with
> identical SQL surface; companion extensions cover
> compression (5 algorithms via compression-multiplexer),
> filter-by-list JOINs (listargs), and persisted SQL function
> definitions (define). Of the three items under
> "Deferred (intentional)", TopoGeometry has since shipped in
> `8fe182b`; what genuinely remains deferred is the
> `postgis-batch` interface (70 fns whose `list<list<u8>> ->
> list<result>` shape doesn't map to scalar SQL  the scalar +
> aggregate forms already cover the SQL semantics) and the two-
> band raster `st_map_algebra2` (needs expression-language work
> not yet started).

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

## Tier 3 reality check (post-vtab landing)

After vtab dispatch shipped, two of the tier-3 entries turned
out to be already-available:

- **fts5**: `libsqlite3-sys` 0.30.1's `bundled` feature compiles
  sqlite3.c with `-DSQLITE_ENABLE_FTS5`. `CREATE VIRTUAL TABLE
  docs USING fts5(title, body); SELECT title FROM docs WHERE
  docs MATCH 'wasm';` works in the cli today with no extension
  loaded. No port needed.
- **rtree**: same path  `-DSQLITE_ENABLE_RTREE` is in libsqlite3-
  sys's bundled flag set. `CREATE VIRTUAL TABLE bbox USING
  rtree(id, minx, maxx, miny, maxy)` and overlap-window queries
  work out of the box. No port needed.
- **geopoly**: NOT in libsqlite3-sys's bundled compile (no
  `-DSQLITE_ENABLE_GEOPOLY`). Would need either a fork of
  libsqlite3-sys with the flag added or a custom build. Small
  scope to enable; useful for tile-shape index queries.

The remaining tier-3 spend therefore reduces to: geopoly enable
(small) + whatever PostGIS-grade geo coverage the user wants.

## PostGIS bridge (newly identified path)

`~/git/postgis-wasm/` already ships `postgis-composed.wasm` with
**317 PostGIS functions** implemented (per the repo's
POSTGIS_FUNCTIONS.md tracking file). The component imports a
substantial geo-tool ecosystem (geos-wasm / proj / mvt /
flatgeobuf / kml / gml / ttf-parser / ...) all from sibling
git/ repos.

A `postgis-sqlite-bridge` extension can adapt those to our
`sqlite:extension/minimal` world:

  * Bridge's `manifest.describe()` declares ~300+ scalar funcs
    (ST_MakePoint, ST_AsText, ST_Distance, ST_Intersects, ...).
  * `scalar_function.call(func_id, args)` routes by id into the
    matching postgis-wasm export.
  * Geometry values cross the boundary as BLOB (EWKB binary
    encoding  what PostGIS itself uses on the wire).
  * Composed once via `wac` so the cli only loads one bundle.

This sidesteps the "port from scratch" treadmill and lights up
PostGIS-class SQL geo support immediately. It's the same
"reuse upstream, wrap the surface" strategy that worked for
fts5/rtree, just at a different layer.

## Final state (delivered)

| Catalog                       | Count  | Source                             |
|-------------------------------|-------:|------------------------------------|
| json1 scalars                 |    13  | extensions/json1                   |
| math scalars                  |    32  | extensions/math                    |
| crypto scalars                |     8  | extensions/crypto                  |
| uuid scalars                  |     3  | extensions/uuid                    |
| regexp scalars                |     4  | extensions/regexp                  |
| stats aggregates              |    14  | extensions/stats                   |
| csv vtab                      |     1  | extensions/csv                     |
| postgis geometry              |   235  | extensions/postgis-bridge          |
| postgis geography             |    32  | extensions/postgis-bridge          |
| postgis aggregates            |     8  | extensions/postgis-bridge          |
| postgis-sfcgal (wrapped)      |    15  | extensions/postgis-bridge          |
| direct sfcgal-wasm            |    21  | extensions/postgis-bridge          |
| postgis raster                |    60  | extensions/postgis-bridge          |
| postgis raster aggregate      |     1  | extensions/postgis-bridge (R6)     |
| postgis topology              |    23  | extensions/postgis-bridge          |
| postgis topogeometry          |     7  | extensions/postgis-bridge          |
| postgis STRtree (scalar API)  |     8  | extensions/postgis-bridge          |
| postgis operators             |     5  | extensions/postgis-bridge          |
| postgis geocoder (parse-only) |     2  | extensions/postgis-bridge          |
| raster_polygon_dump vtab      |    +1  | extensions/postgis-bridge          |
| **postgis-bridge subtotal**   | **419 scalar + 9 agg + 1 vtab** | (composed against postgis-composed.wasm + sfcgal.component.wasm) |
| ieee754 scalars               |     5  | extensions/ieee754                 |
| decimal scalars               |     5  | extensions/decimal                 |
| decimal aggregates            |     1  | extensions/decimal                 |
| vec scalars (sqlite-vec port) |    14  | extensions/vec                     |
| generate_series TVF (eponymous vtab) | +1 | extensions/series              |
| vec0 wrapping kNN vtab        |    +1  | extensions/vec0 (5 backends, persist) |
| vec_each TVF (eponymous vtab) |    +1  | extensions/vec_each                |
| compress scalars (5 algos)    |     5  | extensions/compress                |
| listargs TVF (eponymous vtab) |    +1  | extensions/listargs                |
| define scalars                |     4  | extensions/define                  |
| bloom scalars                 |     5  | extensions/bloom                   |
| hll (agg) + cardinality/merge |     1 agg + 3 scalar | extensions/hyperloglog |
| count_min (agg) + estimate/merge | 1 agg + 3 scalar | extensions/count_min |
| closure graph vtab            |    +1  | extensions/closure                 |
| trie prefix vtab              |    +1  | extensions/trie                    |
| codecs (cbor/msgpack/yaml)    |     6  | extensions/codecs                  |
| sql_normalize scalar          |     1  | extensions/text-utils              |
| prefixes eponymous TVF        |    +1  | extensions/text-utils              |
| spellfix1 fuzzy-match vtab    |    +1  | extensions/spellfix1               |
| time/date scalars (chrono)    |     8  | extensions/time                    |
| crypto-auth (jwt+totp+a2+bcrypt) | 13  | extensions/crypto-auth             |
| web-parsers (jsonpath+html)   |     7  | extensions/web-parsers             |
| ipaddr scalars                |     7  | extensions/ipaddr                  |
| fileio (read/write/stat)      |     7  | extensions/fileio                  |
| zipfile vtab                  |    +1  | extensions/zipfile                 |
| http scalars                  |     5  | extensions/http (minimal-http world) |
| geo (h3 + geohash + maidenhead) |  11  | extensions/geo                     |
| ids (ulid + nanoid + snowflake) |   9  | extensions/ids                     |
| crypto-keys (ed25519+x25519+AEAD+merkle) | 13 | extensions/crypto-keys      |
| time-series (time_bucket + gap_fill TVF) | 1 + 1 vtab | extensions/time-series |
| text-nlp (diff/markdown/stem/phonetic) | 5 | extensions/text-nlp           |
| db-utils (schema introspection + EXPLAIN) | 6 | extensions/db-utils       |
| parsers (color + units + financial) | 12 | extensions/parsers              |
| sketches (t-digest + MinHash)   | 4 + 2 agg | extensions/sketches            |
| formats (toml + ini + xml/xpath) | 7 | extensions/formats                  |
| avro (encode + decode)          |     3  | extensions/avro                    |
| bpe (tiktoken cl100k_base)      |     4  | extensions/bpe                     |
| parquet read vtab               |    +1  | extensions/parquet                 |
| arrow IPC read vtab             |    +1  | extensions/arrow                   |
| excel/xlsx/ods read vtab        |    +1  | extensions/excel (calamine)        |
| pmtiles v3 read vtab            |    +1  | extensions/pmtiles (oxigdal)       |
| onnx inference (5 scalars)      |    +5  | extensions/onnx (tract-onnx)       |
| dns_resolve (capability-gated)  |    +1  | extensions/dns + host hickory      |
| detect (slug/lang/mime)         |    +5  | extensions/detect                  |
| baseN (base32 + base58)         |    +4  | extensions/baseN                   |
| url decomposition (9 scalars)   |    +9  | extensions/url                     |
| emoji (count/extract/lookup)    |    +7  | extensions/emoji                   |
| sqlparse (validate/tables/etc)  |    +6  | extensions/sqlparse                |
| template_render (Jinja2)        |    +1  | extensions/template (minijinja)    |
| fts5 vtab                     |   free | libsqlite3-sys bundled flag set    |
| rtree vtab                    |   free | libsqlite3-sys bundled flag set    |
| geopoly vtab                  |    +1  | -DSQLITE_ENABLE_GEOPOLY via        |
|                               |        | LIBSQLITE3_FLAGS env               |
| dbstat vtab                   |    +1  | -DSQLITE_ENABLE_DBSTAT_VTAB        |
| sqlite_stmt vtab              |    +1  | -DSQLITE_ENABLE_STMTVTAB           |
| bytecode vtab                 |    +1  | -DSQLITE_ENABLE_BYTECODE_VTAB      |
| session / changeset C API     |   free | -DSQLITE_ENABLE_SESSION + _PREUPDATE_HOOK |

**Grand SQL surface delivered**: 687 SQL-callable functions
(scalars + aggregates) plus 17 virtual-table modules (csv, fts5,
rtree, geopoly, raster_polygon_dump, dbstat, sqlite_stmt,
bytecode, generate_series, vec0, vec_each, listargs, closure,
trie, prefixes, spellfix1, zipfile), all reachable through
`.load` or directly via the bundled SQLite, on top of the
existing scalar / aggregate / collation / hook / vtab dispatch
the host implements. vec0 ships five backends  brute
force (default), IVF k-means partitioning, HNSW graph, int8-
quantized HNSW (`index=hnsw8`), and binary LSH
(`index=lsh, d_signature=D, n_probes=M`)  with identical SQL
surface and persistent indexes via a `_vec0_index` shadow
table. Online inserts pick up new source rows automatically;
vec0_refresh / vec0_delete provide explicit invalidation.

Original Plan 2 budget was ~4-5 weeks for a "substantial
extension catalog"; the actual delivery beat that by reusing
the postgis-wasm / sfcgal-wasm / geos-wasm ecosystem the user
already built. The vtab dispatch infrastructure (phases 1-4 in
host + cli) was the unique-to-this-project investment; the
extension catalog itself rides on existing wasm components.

## Deferred (intentional)

- **postgis batch interface** (70 fns): `list<list<u8>> ->
  list<result>` doesn't map to scalar SQL. The scalar forms
  applied row-by-row (with optional rtree pre-filter) give the
  same SQL behavior, and the aggregate forms handle set-shaped
  reductions. See `extensions/postgis-bridge/README.md` for
  the rationale.
- **postgis raster two-band map-algebra** (`st_map_algebra2`
  with cross-band formulas): R3 ships single-band precanned ops
  (`scale`, `threshold`, `cliprange`, `linrescale`); the two-
  band variant needs either expression-language work or a
  ListBand argument shape. Not landed.
~~- **postgis-topology-topogeom (6 fns)**: TopoGeometry
  references topology elements across SQL calls.~~ **Shipped
  in `8fe182b`** as 7 scalar functions
  (`st_topogeom_create / close / type / element_count /
  elements / geom / clear`) backed by its own
  `TOPOGEOM_HANDLES` registry. See `PLAN-topogeom.md` for
  the design rationale.

These are mechanical additions when surfaced; nothing
architectural blocks them.
