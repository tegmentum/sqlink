# Plan: finish the embed catalog

Three parallel tracks to take the embed catalog from 68 / 83
scalar-only to full coverage of scalars + aggregates + vtabs.

Tracks are independent — Track 1 doesn't block anything; Track 2
and Track 3 each need a one-time extension to `sqlite-embed`,
then a sweep of per-extension ports.

| Track | Adds | Unblocks | Estimate |
|---|---|---|---|
| 1 — easy wins | 12 scalar-only ports | (none) | ~3 hours hand + 1 batch of parallel agents |
| 2 — aggregate contract | `register_aggregates` helper in `sqlite-embed`, then 4 ports | hyperloglog, count_min, sketches, decimal | ~1 day helper + ~3 hours per-ext |
| 3 — vtab contract | `register_vtabs` helper in `sqlite-embed`, then ~10 ports | define, pmtiles, text-utils, time-series, vec0, … | ~3 days helper + ~half-day per-ext |

After all three: every scalar / aggregate / vtab extension in the
catalog is composable via `sqlite-wasm-run compose --embed …`.

---

## Track 1 — finish the 12 pure-scalar holdouts

These are routine ports following the contract already documented
in `PLAN-embed-extensions.md`. No new infrastructure needed.

### Targets, sorted by complexity

| Order | Extension | Lines | Pattern | Risk |
|---:|---|---:|---|---|
| 1 | `onnx` | 237 | Stub  stateful ML inference (tract-onnx HashMap of loaded models). Returns "embed path doesn't support session state" error. Like dns/eval/http. | low |
| 2 | `avro` | 255 | Likely direct call-site translation. Apache Avro encode/decode. | low |
| 3 | `web-parsers` | 266 | HTML/markdown parsers; algorithm in external crates. | low |
| 4 | `extfns` | 291 | SQLite ext/misc/extension-functions.c port. Many small helpers. May need to extract a few helpers to crate level. | medium |
| 5 | `time` | 384 | Time/date funcs. Watch for `std::time` use  may need wasi p1 adapter like `ids`. | medium |
| 6 | `crypto-auth` | 398 | HMAC variants (sha1/256/512). Mostly mechanical. | low |
| 7 | `parsers` | 442 | Generic parsers (CSV, TSV, JSON5?). | medium |
| 8 | `geo` | 460 | Geo math. Many trig fns; mostly mechanical. | low |
| 9 | `vec` | 481 | Vector primitives (NOT vec0). Cosine sim, dot product, etc. | low |
| 10 | `crypto-keys` | 483 | Key encoding (PEM, DER, JWK). | medium |
| 11 | `formats` | 492 | Format conversion. Likely many small dispatches. | medium |
| 12 | `text-nlp` | 664 | NLP (tokenize, stem, etc.). Largest in catalog. | medium-high |

### Workflow per port

For each:
1. Read `extensions/<name>/src/lib.rs`  identify scalar entries, FIDs,
   and how the `call()` body dispatches.
2. Decide: is the algorithm already crate-level (delegate via
   `crate::*` like `bloom`/`sha3`/`uuid`/`compress`) or inside
   `wasm_export` (duplicate into `embed.rs` like `setops`, or
   hoist with `pub mod data;` like the proper fix for
   `country`/`phone-prefix`)? Default: delegate if possible,
   duplicate if 30 lines, hoist if 100+.
3. Write `src/embed.rs` (use `extensions/setops/src/embed.rs` as
   template).
4. Update `Cargo.toml` (drop `[workspace]`, add `[features] embed`,
   add `libsqlite3-sys` + `sqlite-embed` optional deps).
5. Update `src/lib.rs` (add `#[cfg(feature = "embed")] pub mod embed;`,
   gate `wasm_export` with `not(feature = "embed")`).
6. Update `cli/Cargo.toml` (path dep + `embed-<name>` feature).
7. Update `cli/src/lib.rs` (register block in
   `register_embedded_extensions`).
8. Test: `cargo build --features embed-<name> --target wasm32-wasip2`.
9. Smoke: `sqlite-wasm-run compose --embed <name>` + a `SELECT`.

### Parallel agents

When agent sessions reset, spawn pairs:
- (onnx, avro), (web-parsers, extfns)
- (time, crypto-auth), (parsers, geo)
- (vec, crypto-keys), (formats, text-nlp)

6 agents = 12 ports. Same pattern as batches 7-8 in the rollout
log. Expect ~10/12 to land clean; finish stragglers by hand.

### Done when

- `sqlite-wasm-run compose --list` shows 80 entries (the 68 already
  shipped + 12 new).
- `make ext NAME=<each>` still produces a clean wasi component
  (the gate cfg ensures embed and WIT can co-exist in the crate).
- 45/45 ext smokes + 4/4 cli smokes pass.

---

## Track 2 — `register_aggregates` + 4 ports

### Pre-requisite: extend `sqlite-embed`

SQLite aggregate functions need three callbacks (or four for
window functions): xStep (per row), xFinal (compute result), and
optionally xValue + xInverse (for window mode). Each aggregation
gets a per-call state buffer via `sqlite3_aggregate_context`.

#### New shapes

```rust
// sqlite-embed/src/lib.rs

/// Per-extension state type. Trait so each extension defines its
/// own concrete state struct (HLL register bank, t-digest centroids,
/// running sum, …).
pub trait AggregateState: Default + Send {
    /// Called once per row by the generic step thunk. Receives the
    /// row's args; returns Err to abort the aggregation.
    fn step(&mut self, args: &[SqlValueOwned]) -> Result<(), String>;
    /// Called once at end of aggregation. Returns the SQL value
    /// (often a BLOB containing serialized state).
    fn finalize(&self) -> Result<SqlValueOwned, String>;
}

pub struct AggregateSpec {
    pub func_id: u64,
    pub name: &'static [u8],
    pub num_args: i32,
    pub deterministic: bool,
    /// Constructor for per-aggregation state. Returned Box is
    /// stored in sqlite3_aggregate_context for the duration of
    /// the aggregation.
    pub make_state: fn() -> alloc::boxed::Box<dyn AggregateState>,
}

pub unsafe fn register_aggregates(
    db: *mut sqlite3,
    specs: &[AggregateSpec],
) -> c_int;
```

#### Generic thunks

```rust
// One step_thunk for every (extension, agg) pair. Reads the state
// pointer out of sqlite3_aggregate_context, constructs it on first
// call, dispatches to AggregateState::step.
unsafe extern "C" fn step_thunk(
    ctx: *mut sqlite3_context,
    argc: c_int,
    argv: *mut *mut sqlite3_value,
) { … }

// One final_thunk; calls AggregateState::finalize, writes result,
// frees the state Box. Sqlite calls this even if step never fired
// (zero-row aggregation), so the state ctor must be cheap.
unsafe extern "C" fn final_thunk(ctx: *mut sqlite3_context) { … }
```

#### State threading via `sqlite3_aggregate_context`

```rust
// On first call to step: ask sqlite for an opaque ptr-sized slot,
// Box::into_raw a fresh AggregateState into it.
// On subsequent calls: re-read the pointer, mutate the state.
// On final: read pointer, finalize, free.
unsafe fn state_ptr<S: AggregateState>(
    ctx: *mut sqlite3_context,
    make: fn() -> Box<dyn AggregateState>,
) -> *mut S {
    let slot = sqlite3_aggregate_context(
        ctx,
        core::mem::size_of::<*mut ()>() as c_int,
    ) as *mut *mut dyn AggregateState;
    if (*slot).is_null() {
        *slot = Box::into_raw(make());
    }
    // Downcast: AggregateState is unsized but for our use we
    // always cast back to the per-extension type. Per-extension
    // adapters handle the unsize→sized roundtrip.
    *slot as *mut S
}
```

Trickier than scalars because:
- `dyn AggregateState` is unsized; can't pun-cast through
  `sqlite3_aggregate_context`'s ptr slot directly. Two options:
  (a) one-level indirection via `Box<dyn ...>` stored at the slot,
  (b) per-extension typed thunks that bypass `dyn`.
- Window functions add xValue + xInverse; defer for v1, add later
  as `WindowAggregateSpec` with extra `inverse` callback.

#### Validation

- Unit test in `sqlite-embed` with a trivial counter aggregate
  (xStep += 1, xFinal returns count).
- Smoke against existing `count_min` / `hyperloglog` once ported.

### Per-extension ports (sorted by complexity)

| Order | Extension | Aggregates | Notes |
|---:|---|---:|---|
| 1 | `decimal` | 1 (`decimal_sum`) | Simple running sum of decimal strings. Algorithm already at crate level. Good first port. |
| 2 | `hyperloglog` | 1 (`hll`) | Fixed-size register bank (16384 bytes). State = Box<[u8; 16384]>. |
| 3 | `count_min` | 1 (`count_min`) | Fixed-size sketch (32 KB). Similar to HLL. |
| 4 | `sketches` | 2 (`t_digest`, `minhash`) | Two aggregates with different state shapes. t-digest needs centroid list (Vec). |

`stats` and `postgis-bridge` not on the list because:
- `stats` is **14 aggregates** (median/percentile/skewness/regr_*/…) — feasible but a lot of repetition. Defer until after the helper is proven on the simpler 4.
- `postgis-bridge` is 4059 lines + needs a vtab too — its own project.

### Estimated effort

- `register_aggregates` helper: 1 day to design + implement + test
  the `Box<dyn ...>` lifecycle through `sqlite3_aggregate_context`.
- Per port: ~2-3 hours each (more than scalars because the state
  type design needs thought).
- Total Track 2 timeline: ~3 days for 4 ports + helper.

### Done when

- `sqlite-wasm-run compose --list` includes hyperloglog, count_min,
  sketches, decimal.
- Embed bench shows ~zero overhead per row vs WIT (aggregates
  amortize the boundary cost better than scalars but the
  improvement is real).
- 45/45 + 4/4 smokes pass; existing 5 stateful-world smokes
  continue to pass since the WIT path is untouched.

---

## Track 3 — `register_vtabs` + ~10 ports

### Pre-requisite: extend `sqlite-embed` (or new crate `sqlite-embed-vtab`)

Virtual tables are the largest surface: `sqlite3_module` has 22
function pointer slots in modern sqlite. Most extensions only
need a subset, but the embed contract still has to provide them.

#### Decision: separate crate vs same crate

The vtab API is large enough that bolting it onto `sqlite-embed`
makes the crate's surface awkward. Recommendation: **new crate
`sqlite-embed-vtab`** depending on `sqlite-embed` for the
`SqlValueOwned` etc. Each extension's `Cargo.toml` gains a
`vtab` feature pulling in `sqlite-embed-vtab`.

#### Minimum viable surface

For read-only eponymous vtabs (the common case  series, completion,
listargs, pmtiles, text-utils, vec_each, vec0 read paths), we
need:

```rust
// sqlite-embed-vtab/src/lib.rs

pub trait Vtab: Send + Sync {
    /// CREATE TABLE schema string ("CREATE TABLE x(a, b HIDDEN, c)").
    fn schema(&self) -> &str;
    /// Plan a query against this vtab. Maps usable constraints to
    /// argv positions used in `filter`.
    fn best_index(&self, info: &mut BestIndexInfo) -> Result<(), String>;
    /// Open a cursor. Cursor lifetime managed by the helper.
    fn open(&self) -> Result<Box<dyn VtabCursor>, String>;
}

pub trait VtabCursor: Send {
    fn filter(&mut self, idx_num: i32, idx_str: Option<&str>, args: &[SqlValueOwned]) -> Result<(), String>;
    fn next(&mut self) -> Result<(), String>;
    fn eof(&self) -> bool;
    fn column(&self, col: i32) -> Result<SqlValueOwned, String>;
    fn rowid(&self) -> Result<i64, String>;
}

pub struct VtabSpec {
    pub name: &'static [u8],
    pub eponymous: bool,
    pub make: fn() -> Box<dyn Vtab>,
}

pub unsafe fn register_vtabs(
    db: *mut sqlite3,
    specs: &[VtabSpec],
) -> c_int;
```

#### What the helper has to wire

For each `VtabSpec`, build a `sqlite3_module` instance with
function pointers that:
1. `xConnect` — call `make()`, store `Box<dyn Vtab>` in
   `sqlite3_vtab` userdata.
2. `xDisconnect`/`xDestroy` — free the Box.
3. `xBestIndex` — translate `sqlite3_index_info*` to `BestIndexInfo`,
   call `Vtab::best_index`, translate back.
4. `xOpen` — call `Vtab::open`, store `Box<dyn VtabCursor>` in
   `sqlite3_vtab_cursor`.
5. `xClose` — free cursor Box.
6. `xFilter` — extract argv, call `VtabCursor::filter`.
7. `xNext`/`xEof`/`xColumn`/`xRowid` — direct delegation.

Write paths (`xUpdate`) — defer to v2. Read-only covers the
most common cases.

#### What's NOT in v1

- Write paths (`xUpdate`, `xBegin`, `xCommit`, `xRollback`).
- Shadow tables (`xShadowName`).
- Renames (`xRename`).
- Find function (`xFindFunction`) for vtab-bound functions.

If any extension's primary path uses these, it stays on the WIT
loader.

#### Validation

- Unit test in `sqlite-embed-vtab` with a `series` clone — 3 hidden
  args (start, stop, step), one column.
- Smoke against existing `series` once ported.

### Per-extension ports (sorted by viability)

Of the 18 vtab extensions, target the read-only / eponymous ones
first.

| Order | Extension | Vtab | Notes |
|---:|---|---|---|
| 1 | `series` | `generate_series` | The canonical vtab smoke. ~50 LOC port. |
| 2 | `listargs` | `listargs` | Scaffold ext used elsewhere as a template. |
| 3 | `define` | `define` | scalar+vtab; user-defined functions over SQL fragments. |
| 4 | `completion` | `completion` | shipped phases 1-7. Read-only. |
| 5 | `trie` | `trie` | Likely read-only. |
| 6 | `vec_each` | `vec_each` | Vec0's per-component eponymous helper. |
| 7 | `text-utils` | various | Read-only by design. |
| 8 | `pmtiles` | `pmtiles_read` | File-backed read; may need wasi-filesystem coordination. |
| 9 | `time-series` | various | Aggregation-flavored read paths. |
| 10 | `vec0` | `vec0` | Full kNN vtab. Largest individual port. Read paths only in v1. |

Skip in v1:
- `arrow`, `parquet` — likely need write paths or use external schema
- `closure`, `csv`, `excel`, `zipfile`, `spellfix1` — write paths
- `postgis-bridge` — own project (4059 lines, scalar+agg+vtab)

### Estimated effort

- `sqlite-embed-vtab` helper: ~3 days. Most of the work is in
  `xBestIndex` translation (sqlite3_index_info has 12+ output
  fields) and the cursor lifecycle through Rust's borrow checker.
  Best-index is the part where every bug in the helper bites
  hard — needs careful unit testing.
- Per port: ~half-day each. Vtabs have more boilerplate per
  extension than scalars even with the helper.
- Total Track 3 timeline: ~2 weeks for helper + 6-10 ports.

### Done when

- `sqlite-embed-vtab` published as a workspace member.
- `series` embedded and yields `SELECT value FROM generate_series(1,5)`
  returning 1..5 in the embedded cli.
- Existing 18 vtab WIT smokes still pass (read-only path unchanged).
- Cli with `series`, `define`, `completion`, `text-utils`,
  `vec_each`, `pmtiles`, `time-series`, `vec0` embedded all compose
  cleanly.

---

## Phasing recommendation

Suggested order to maximize incremental value:

1. **Track 1 in parallel with Track 2 helper design.**
   The 12 easy wins ship while sqlite-embed is being extended
   for aggregates  no cross-blocking.
2. **Track 2 ports** after the helper lands. 4 ports total; each
   one validates the helper from a different angle (state shape
   variation across hll register bank vs CMS sketch vs t-digest
   centroids).
3. **Track 3 helper** is the biggest investment. Defer until
   Tracks 1+2 are done so the team's understanding of the
   embed pattern is mature.
4. **Track 3 ports** in `series`  vec0 order. The first port
   of each new helper finds bugs the next ports avoid.

After all three tracks, the catalog reads:
- ~80 scalar extensions embeddable (the 12 + the 68 already)
- ~5 aggregate extensions embeddable (the 4 + maybe `stats`)
- ~8-10 vtab extensions embeddable

That gets us from 68/121 (56%) of the whole catalog to ~95/121
(~78%) embeddable. The remaining 26 are: extensions that need
write-path vtabs, the 3 still-blocked (template/graphql/ids), and
the 2 extensions that are scaffolds (`postgis-bridge` and possibly
one or two custom ones). Honest stopping point for the embed
project.

---

## What this plan deliberately doesn't include

- **`postgis-bridge`**: 4059-line composite. Defer to its own
  project.
- **`uint` collation**: 1 extension. Could ship a
  `register_collations` helper in `sqlite-embed`  ~half-day for
  the helper plus a 30-min port. Add as Track 4 if needed.
- **Window aggregates**: defer to `AggregateSpecV2` once basic
  aggregates work.
- **Write-path vtabs** (`xUpdate`/`xBegin`/`xCommit`): the
  read-only majority covers most consumer needs. Add when a
  consumer pulls.
- **Cross-component composability** of the helpers themselves
  (e.g. an embed extension that declares it depends on another's
  scalars). Not the embed project's problem.

---

## Reference points

- `PLAN-embed-extensions.md` — the existing scalar contract +
  rollout log.
- `extensions/setops/src/embed.rs` — current best small reference.
- `extensions/sha3/src/embed.rs` — small reference with hex output.
- `extensions/uuid/src/embed.rs` — small reference with
  determinism flag variants.
- `extensions/bloom/src/embed.rs` — reference for crate-level
  helpers (model for Track 2 state-type design).
- `sqlite-embed/src/lib.rs` — the helper crate to extend in
  Tracks 2 and 3.
