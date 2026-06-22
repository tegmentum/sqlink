# Plan: postgis-bridge performance optimization

> **Status (2026-06-15)**: O1 shipped (commit `124116d`).
> Measured profile after O1 led to **dropping O2 and O3 as
> not-worth-doing**.
>
> Measured per-call cost on a 1000-row scan with the cached
> Store + Instance:
>
> | Test                                                  | Per call |
> |-------------------------------------------------------|----------|
> | `SUM(st_x(g))` (trivial)                              | ~75 µs   |
> | `SUM(st_distance(g, const_polygon_blob))` (102 B blob)| ~89 µs   |
> | `SUM(st_distance(g, st_makepoint(...)))` (subexpr)    | ~135 µs  |
>
> The plan's own gate for O2 was "if `from_wkb` is the new top
> frame, do O2." It isn't. `from_wkb` on a 102-byte polygon
> accounts for ~14 µs of an 89 µs call — about 16% of per-call
> time. The closure refactor of 108 call sites for a 16%
> best-case improvement on heavy-WKB calls doesn't pay back.
> Wasm boundary overhead (call setup, args marshaling) is now
> the floor and isn't addressable by interning.
>
> O3 (slim bundle) was conditional on cold-start being a real
> complaint. No evidence it is. Skip.

## Goal

Cut per-call cost for the postgis-bridge dispatch path so a
`SELECT st_distance(a, b) FROM big_table` query scales sanely.
Three independent optimizations, ordered by ROI; each phase is
self-contained and can ship separately.

## Current state

Bridge surface: 421 scalars + 9 aggregates + 1 vtab against a
~100 MB composed bundle (postgis + sfcgal + proj + gdal + geos +
mvt + flatgeobuf + kml + geobuf + marc21 + gml + ttf + rustybuzz
+ geographiclib). Per scalar call, `host::Host::dispatch_scalar`
(`host/src/lib.rs:2208`) does:

1. Look up the LoadedExtension by name (RwLock<HashMap> read).
2. `make_loaded_linker(&engine)` — fresh Linker, re-registers
   all wasi:* + sqlite:extension/* host functions.
3. `build_loaded_store(&engine, &ext, db_path)` — fresh Store +
   LoadedState (allocates per-call host fixtures).
4. `loaded::Minimal::instantiate_async(&mut store, &component,
   &linker)` — instantiate from the pre-compiled component:
   build memories / tables / globals / VFS state / wit-bindgen
   resource tables from scratch.
5. Convert SqlValue args across the two bindgens' types.
6. `call_call(&mut store, func_id, &loaded_args).await` — the
   actual scalar dispatch.
7. Drop the Store; everything from steps 2-4 is thrown away.

For the postgis bundle, step 4 dominates. The
already-existing `CachedStateful` (aggregates) and
`CachedTabular` (vtab modules) avoid steps 2-4 by parking
(Store, Instance) under a per-extension mutex; scalars don't
yet do this.

## Three optimizations (independent phases)

### O1 — Cached scalar Store+Instance (HIGH ROI)

The single biggest lever. Mirror `CachedStateful` exactly.

#### Architectural questions

**Q1.1. Same per-extension mutex granularity, or per-thread?**

Per-thread would give true parallelism on multi-row queries
inside the cli's executor, but the cli is single-threaded
through SQLite's connection (the connection lives on one
thread, callbacks are sync from SQLite's perspective). Per-
extension matches what stateful + tabular already do and keeps
the LoadedState invariants (thread_locals in the guest hold
across calls).

**Decision**: per-extension mutex, identical pattern to
`stateful_locked` / `tabular_locked`.

**Q1.2. Engine epoch / fuel reset per call?**

Stateful and tabular don't reset fuel between step calls — the
aggregator is allowed to consume an unbounded fuel budget. For
scalars, allowing cumulative fuel usage across rows would let
one query starve another (no per-row reset = a buggy guest
exhausts fuel partway through a million-row scan and the rest
of the query traps).

**Decision**: bump the store's epoch and refill fuel before
each `call_call`. The bumper thread already runs at the engine
level — we just need a per-call fuel refill via
`store.set_fuel`.

**Q1.3. What about thread_local invariants in the guest?**

The bridge today uses thread_locals deliberately — AGGS,
TOPO_HANDLES, TOPOGEOM_HANDLES, RPD_INSTANCES, RPD_CURSORS.
Caching the Store means those thread_locals survive across SQL
rows in the same connection. For TOPO* and STRtree handles
this is REQUIRED (the user opens a handle in one statement and
uses it in the next); for AGGS the existing
`stateful`-world path already gets caching, so this is just
about handles. The change is strictly a perf win that also
happens to fix a latent correctness issue: today scalar handle
ops "work by accident" because the cli reuses the same scalar
call path on the same thread, but in principle a fresh Store
would lose the handles.

**Decision**: explicitly document that scalar dispatch now
shares a per-extension cached Store. Handles persist across
SQL calls. (No behavioural change for current callers; they
already rely on this.)

#### Implementation

- Add `CachedMinimal { store, instance }` struct next to
  `CachedStateful` / `CachedTabular`.
- Add `cached_minimal: Arc<tokio::sync::Mutex<Option<CachedMinimal>>>`
  field on `LoadedExtension`. Init `None`.
- Add `Host::minimal_locked(ext_name)` — exact copy of
  `tabular_locked`, retargeting at `loaded::Minimal` and
  `make_loaded_linker`.
- Rewrite `dispatch_scalar` to call `minimal_locked`, then
  refill fuel + bump epoch on the cached Store before
  `call_call`.
- Build-store helper unchanged.

#### Risk

LoadedState contains an Arc<Mutex<Option<SqliteConn>>> for the
SPI live-re-entry path. Keeping the same LoadedState across
calls means the SPI conn handle is shared between rows. That's
fine for read-only SPI use; for SPI writes the existing
mutex already serializes.

Fuel-related guest panics or trap-on-OOM in the cached Store
would normally invalidate the Store. The cached aggregate path
already handles this via the Mutex<Option<...>>; on error the
Option is reset to None and the next call rebuilds. Mirror
that.

#### Estimated effort

~3-4 hours. Pattern is fully established.

### O2 — Geometry intern API (MEDIUM ROI, additive)

Once O1 lands, the bulk of per-call cost shifts to bridge-side
work — primarily `Geometry::from_wkb(blob)` for every input,
done inside the bridge's wasm. For chained ops (`SELECT
st_distance(g, ref_geom) FROM t WHERE st_intersects(g,
ref_geom)` — `ref_geom` parses twice per row even with caching)
or repeated joins against a fixed set of polygons, the parse is
real cost.

#### Architectural questions

**Q2.1. Transparent content-hash cache vs explicit handle API?**

Transparent: bridge maintains an LRU<u64, Geometry> keyed by
xxhash(wkb). The `from_wkb` helper checks the LRU first;
existing scalars get free dedup. Pro: zero API churn. Con:
forces the Geometry resource to live across the helper return;
either every call site refactors to a closure form or we keep
the LRU value type as Geometry and accept owned-handle
duplication (one Geometry per cache hit).

Explicit: add `st_geom_intern(wkb) -> INTEGER` (handle) and
parallel handle-flavored versions of every common scalar. Pro:
no refactor of existing fns. Con: doubles the surface area for
common predicates.

**Decision**: transparent. The Geometry resource clone is
cheap (it's just a u32 resource handle in wit-bindgen's table),
and we already have the closure-around-borrow pattern
established by `with_topo_handle` / `with_topogeom_handle`.
Refactor the existing `from_wkb` call sites to a
`with_geom_from_wkb` closure form in a single mechanical pass.

Side benefit: NULL-arg geometries get dedup'd as the well-known
"empty geometry" sentinel.

**Q2.2. LRU capacity?**

Big enough to cover typical join sets (one fixed polygon
joined against millions of rows), small enough to not blow the
wasm heap on a megabyte-WKB geometry. Default 64 entries with
no per-entry size cap; oversized geometries naturally evict
peers but stay cached themselves.

**Decision**: capacity 64, no size cap. Plumb a per-extension
`config::set` to override (`postgis.geom_lru_capacity=N`).

**Q2.3. Hashing strategy?**

WKB blobs are 21+ bytes (point) up to multi-MB (complex
multipolygon). xxh3 is the right tool — non-crypto, ~10
GB/s on M-series Macs.

**Decision**: `twox-hash 2.x` with the `xxhash3_64` family.
Pure-Rust, builds wasm32-clean.

#### Implementation

- Add `twox-hash` dep to bridge Cargo.toml.
- Add thread_local LRU registry: `RefCell<LinkedHashMap<u64,
  Geometry>>` (insertion-order LRU; capacity = configurable).
- Rewrite `fn from_wkb(bytes: &[u8], name: &str) -> Result<Geometry, String>`
  to consult the LRU first; on miss, parse + insert + evict
  oldest if over capacity.
- The call-site refactor isn't actually needed — `from_wkb`
  still returns owned Geometry and the LRU keeps a clone.
  Wait — Geometry isn't Clone.

  Re-think: the cache value can't be `Geometry`; it has to be
  the WKB bytes (which we already have), and the cache speeds
  up by skipping the `from_wkb` parse. But the upstream call
  STILL needs a Geometry resource per call.

  Hmm. Then the cache doesn't help — we still parse on every
  call to construct the resource. Unless we keep the Geometry
  resource alive (one per cache entry) and pass `&Geometry`
  to upstream.

  This needs the closure refactor after all. Acceptable cost:
  ~60 call sites, mechanical.

  Alternate: cache the resource handle (`Resource<Geometry>`)
  if wit-bindgen exposes that. Worth a brief spike.

**Pivot**: spike first, decide between closure-refactor and
resource-handle-cache. Time-box to 30 min; pick whichever is
less invasive. If both are messy, drop O2 and accept that
parse-per-call is the noise floor once O1 lands.

#### Estimated effort

~1 day (with the spike). Falls to half a day if the resource-
handle-cache works without refactor.

### O3 — Slim bundle variants (LOW ROI, niche)

The 100 MB `postgis_full.wasm` carries every interface even
for callers that only need ST_Distance. Per-instantiation
allocation is amortized once O1 lands, but cold-start (CLI
launch → first query) still pays the full parse+validate cost.

#### Architectural questions

**Q3.1. What variants?**

- `slim`: postgis-wasm + geos-wasm + geographiclib-wasm
  (geometry + measurements + predicates + processing + WKT/WKB
  IO + geodesic distance). No proj, no gdal, no sfcgal, no
  raster, no flatgeobuf/kml/gml/marc21/ttf/rustybuzz. ~25-30
  MB estimated.
- `geo`: slim + proj (cross-CRS transforms work; raster
  remains absent). ~35 MB.
- `full`: today's bundle. Default.

**Decision**: ship `slim` + `full`. `geo` is the natural middle
but not enough callers will know they need it; defer.

**Q3.2. Build pipeline?**

Compose script lives in `~/git/postgis-wasm/scripts/compose.sh`
and takes plug paths via env. Either:

- Add an upstream `BUILD_VARIANT=slim|full` env that selects
  the plug list, and publish two artifacts.
- Keep a single artifact and accept that anyone wanting slim
  composes their own.

**Decision**: add a `BUILD_VARIANT` env in postgis-wasm's
compose.sh + ship `postgis-slim.wasm` and
`postgis-composed.wasm` alongside each other. Bridge compose
in this repo grows a matching `BUNDLE=slim|full` env.

**Q3.3. How does the CLI choose?**

For users who download via the CAS cache (`.load
https://extensions.<your-org>.dev/postgis-slim.wasm`), URLs
distinguish. For local builds, env at compose time. No
runtime selection.

**Decision**: variants are build-time artifacts; runtime sees
one wasm. CLI's `.load` URL is the selector.

#### Implementation

- Edit `~/git/postgis-wasm/scripts/compose.sh` to gate plug
  inclusion on `BUILD_VARIANT`.
- Edit (or duplicate) `extensions/postgis-bridge`'s compose
  step to do the same for the bridge-side compose.
- Build both variants; sanity-check `wasm-tools component wit`
  on the slim one to confirm reduced imports.
- Bridge manifest stays unchanged: a slim bundle just returns
  errors for fns whose upstream interface isn't satisfied. (Or
  we add a manifest-shrinking pass — but that's plan-level
  surgery, defer.)

#### Risk

The bridge manifest declares 421 scalars regardless of bundle
variant. A slim bundle that's missing the raster-types
interface would fail to instantiate at all (unsatisfied
import). So slim needs the bridge to be re-composed with the
slim plug list, OR the bridge needs conditional manifest
entries based on bundle inspection at load time.

**Decision**: per-variant bridge wasm. Two artifacts. Document
the matrix.

#### Estimated effort

~half day. Most work is shell + docs.

## Total estimated effort

- O1: 3-4 hours (highest ROI; do first)
- O2: 4-8 hours (do after O1 if benchmarks justify)
- O3: 4 hours (niche; do only if cold-start becomes a
  complaint)

~2 days end-to-end for all three.

## Sequencing

1. **O1 first, ship alone, measure.** Take a representative
   query (`SELECT count(*) FROM rows WHERE st_intersects(g, ?)`
   on 100K rows) and clock before/after. The number tells you
   whether O2 is worth pursuing.
2. **Run a profile.** With O1 in, profile the cached-instance
   path. If `from_wkb` is the new top frame, do O2. If not,
   skip O2 and look at bundle-size or composition overhead
   instead.
3. **O3 only if size/cold-start is a real complaint.** Don't
   build variants pre-emptively.

## Out of scope

- True parallel scalar dispatch (one Store per thread). The
  cli is single-connection-single-threaded today; multi-
  connection support is a separate Plan-1-style architecture
  change.
- Persistent geometry intern cache across sessions. The LRU is
  thread-local and dies with the process.
- A bridge-level cache of `to_wkb` outputs from prior calls
  (memoizing predicates on the same arg pair). Plausibly the
  next layer up, but reaches into SQL caching territory that
  SQLite's own expression caching already handles.

## Notes

- The `wasm-bench` crate (workspace dev-dep) has a microbench
  harness for component-call overhead — extend it to cover
  scalar dispatch before/after O1 as the perf gate.
- The `host/SPI-LIVE-ARCHITECTURE.md` doc lays out the
  reentrant SPI invariants. Keep in mind when caching the
  scalar Store that SPI re-entry from inside a cached call
  still has to work — same constraint the cached_stateful
  path satisfies.
