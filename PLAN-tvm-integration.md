# Plan: TVM integration for SQLite-in-WASM (lifting the 4 GB wall)

Goal: support SQLite working sets — including page caches,
`:memory:` databases, intermediate query state — substantially
larger than 4 GB, while keeping the wasm32 toolchain and the
existing build story intact.

This is a longer-horizon plan than the parity work in
`PLAN-rust-cli-parity.md`. It depends on `tvm-wasm` (`~/git/tvm-wasm/`),
the Tiered Virtual Memory substrate, and is not blocked on any
cleanup phase. Discrete from CLI work — its own document.

## The 4 GB wall, where it actually bites

For **file-backed** databases via `wasivfs`, we already support
databases far larger than 4 GB today:

- The disk file is arbitrary size (host filesystem permitting).
- SQLite reads pages on demand; only the page cache lives in
  wasm linear memory.
- Default page cache is ~2 MB; a 100 GB database uses ~2 MB of
  wasm memory.

The wasm 4 GB ceiling actually bites in three cases:

1. **`:memory:` databases** — the whole DB lives in linear memory.
2. **Large page caches** — when working-set locality is wider than
   4 GB and a small cache thrashes against the VFS.
3. **Wasm-side state exhaustion** — extension data, intermediate
   join results, sort buffers, lookaside scratch can eat the rest
   of the 4 GB even when the DB file is on disk.

## The four paths to lifting the wall

| | What it does | Cost | What it leaves on the table |
|---|---|---|---|
| **A. Status quo (wasivfs)** | File-backed I/O via host; page cache stays small | Zero | Cases 1–3 above |
| **B. Custom `pcache2`** | Register a TVM-backed page cache via `sqlite3_config(SQLITE_CONFIG_PCACHE2, …)` | Medium | Cases 1 and 3 |
| **C. Custom `malloc`** | Replace `sqlite3_malloc` family via `sqlite3_config(SQLITE_CONFIG_MALLOC, …)` | Higher | Less than B leaves — but every other SQLite subsystem becomes TVM-managed |
| **D. Switch to `wasm64`** | Single 64-bit linear memory | Toolchain risk + 10–20% perf | Tiering, sharing, isolation — TVM's value beyond just address space |

A is what we have today. B and C are the TVM integration paths.
D is an alternative (different tradeoff space, not stacked on TVM).

## Does C replace B?

**No. They are additive.** They cover disjoint subsystems:

| Subsystem | Default impl | Replaced by |
|---|---|---|
| Page cache (largest single consumer in typical workloads) | `pcache1` (internally uses `sqlite3_malloc`) | `SQLITE_CONFIG_PCACHE2` (Path B) |
| Everything else: lookaside, temp tables, sorter, schema, statement bytecode, result buffers, mutexes | `sqlite3_malloc` (default `mem3`/`mem5`) | `SQLITE_CONFIG_MALLOC` (Path C) |

When you register a custom `pcache2`, your impl controls **its own**
allocations — it bypasses `sqlite3_malloc` for page memory. So:

- B alone: page cache is TVM-managed (tiered, > 4 GB); everything
  else still in default malloc (constrained to ~4 GB).
- B + C: page cache is TVM-managed via B; everything else is also
  TVM-managed via C. Both subsystems share the same TVM region
  directory but use different region types (`page-store` for B,
  `heap`/`arena` for C).
- C alone: every allocation goes through TVM malloc, but the
  default `pcache1` calls into that malloc — so the page cache
  ends up TVM-backed too, just less efficiently than via the
  pcache2 path (no typed `page-store` region, no native
  pin/demote semantics for page-level eviction).

**Implication for sequencing**: doing B first is the right move
because it covers the biggest single consumer at the lowest cost.
Doing C later is purely additive — you write a new module, register
a second callback table, no rework of the pcache code.

## Architectural sketch

```
                  ┌──────────────────────────────────┐
                  │   sqlite3.c (unmodified)         │
                  └──────────────┬───────────────────┘
                                 │ uses
                  ┌──────────────┼──────────────────┐
                  │              │                  │
        sqlite3_pcache_methods2  │  sqlite3_mem_methods
        (registered via          │  (registered via
         SQLITE_CONFIG_PCACHE2)  │   SQLITE_CONFIG_MALLOC)
                  │              │                  │
                  ▼              │                  ▼
        ┌────────────────┐       │       ┌────────────────────┐
        │ tvm-pcache     │       │       │ tvm-malloc         │
        │  (Path B)      │       │       │  (Path C)          │
        └───────┬────────┘       │       └─────────┬──────────┘
                │ allocates via  │                 │ allocates via
                ▼                │                 ▼
        page-store region        │            heap / arena regions
                │                │                 │
                └────────────────┴─────────────────┘
                         shared TVM region directory
                         (tvm-guest-mm / tvm-guest-rt)
```

Both Path B and Path C reduce to TVM region allocations against a
shared region directory. The region **types** differ — TVM's
`page-store` region carries semantics (fixed-size slots, page IDs,
explicit pin/demote) that fit the pcache contract exactly; the
malloc layer uses general `heap` regions for arbitrary-size
allocations.

## Phase 1 — Path B: TVM-backed `pcache2`

One commit if measurements come back clean.

### What to build

A small C library (~500 LOC) exposed as `tvm_pcache_init()`. It:

1. Implements all eleven methods of `sqlite3_pcache_methods2`
   (`xInit`, `xShutdown`, `xCreate`, `xCachesize`, `xPagecount`,
   `xFetch`, `xUnpin`, `xRekey`, `xTruncate`, `xDestroy`,
   `xShrink`).
2. For each `xFetch`, allocates a page slot in a TVM `page-store`
   region; returns a pointer to it. The region handle is held
   inside the `sqlite3_pcache` struct.
3. For each `xUnpin`, calls `tvm_region.demote(handle)` — TVM
   handles the actual eviction policy (LRU, tiering to disk).
4. `xRekey` updates the page-id index inside the region; `xTruncate`
   drops a range; `xDestroy` releases the region.

### Registration

```c
extern int tvm_pcache_init(void);
// In our sqlite3 init wrapper, called once before any
// sqlite3_open:
sqlite3_config(SQLITE_CONFIG_PCACHE2, &tvm_pcache_methods);
```

This needs to happen before `sqlite3_initialize`. We control the
init wrapper (`src/sqlite_wasm.c` for the C build,
`core/src/db.rs` opens via raw FFI for the Rust build), so the
hook point exists.

### TVM glue

`tvm-guest-mm` exposes the region API to wasm guests. The pcache
allocates one region per `sqlite3_pcache` instance (one per
database connection's pager); region type = `page-store`; budget
configured from `xCachesize`.

### Validation

- **Functional**: existing host tests pass with `tvm-pcache`
  registered. No regression on the 33 tests we have.
- **Capacity**: open a 10 GB database. Run a query whose hot set
  is 6 GB (joins across multiple wide tables). Configure
  `xCachesize = 8 GB`. Observe that wasm linear memory usage
  stays below 4 GB while TVM's `page-store` region grows past
  4 GB.
- **Perf**: measure overhead vs. default pcache1 on a workload
  whose hot set fits in 4 GB. Target: ≤ 10% slower (region handle
  indirection has a cost; we want it bounded).

### Open questions for Phase 1

- **One region per connection or one shared region across connections?**
  Per-connection is simpler (each `sqlite3_pcache` owns its region).
  Shared is more efficient when multiple connections hit the same
  database. Defer to phase 1.5 if profiling shows it matters.
- **Disk spill destination.** TVM's `page-store` region can spill
  to disk. Where? Same directory as the SQLite db file? Separate
  cache dir? Decided when wiring `tvm-wasmtime`'s spill backend.
- **Pin/demote granularity.** SQLite calls `xUnpin` with hints
  (`discard`-vs-`reuse`). Map directly to TVM's `demote`/`spill`
  primitives.

## Phase 2 — Path C: TVM-backed `sqlite3_malloc`

After Phase 1 lands and measurements show the residual 4 GB
pressure (or to fully solve the `:memory:` database case).

### What to build

`tvm_malloc_init()` — implementations of `sqlite3_mem_methods`
(`xMalloc`, `xFree`, `xRealloc`, `xSize`, `xRoundup`, `xInit`,
`xShutdown`). Routes every allocation through TVM.

### Region strategy

Three TVM regions for the malloc layer:

| Region | Type | Purpose |
|---|---|---|
| `sqlite-heap` | `heap` | General allocations: schema, prepared statements, result buffers |
| `sqlite-arena` | `arena` | Per-statement scratch: temp tables, sorter, hash joins. Reset at statement teardown. |
| `sqlite-lookaside` | `heap` (pinned) | Per-connection lookaside; must be fast. Pinned in tier 0. |

Allocation size + an `eMemSubsystem` hint (SQLite passes it via
`sqlite3MemMalloc`'s caller) decide which region.

### Validation

- **Functional**: same as Phase 1 — no test regression.
- **`:memory:` capacity**: open a 6 GB `:memory:` database. Insert
  rows until the table contains 6 GB of data. Query it. Confirm
  it works and that wasm linear memory stays bounded.
- **Perf**: lookaside hits should be within 5% of native (it's a
  hot path). General malloc overhead ≤ 15%.

### What stays out of scope

- **Mutex subsystem** (`SQLITE_CONFIG_MUTEX`). SQLite has its own
  recursive mutex impl; wasm doesn't need cross-thread mutexes
  for our single-threaded build.
- **VFS** (`SQLITE_CONFIG_VFS`). `wasivfs` is doing its job;
  no reason to TVM-back it.
- **PRNG** / **logging** / etc. — not memory issues.

## Phase 3 — wasm64 alternative

If the integration cost of B + C proves too high, or if the
toolchain situation for `wasm64-wasip2` improves to "production
ready":

- Switch build to `wasm32-wasip2` → `wasm64-wasip2`.
- Drop the 4 GB ceiling at the engine level.
- No code changes in SQLite or our crates beyond build flags.
- Pay ~10–20% perf, lose tiering / spill / typed regions.

This is the alternative path, not a follow-on. If we commit to
TVM, we commit to staying on wasm32 and letting TVM do the work.

## What's published as crates

| Crate | Role |
|---|---|
| `sqlite-pcache-tvm` | C source for the Path B pcache2 impl + Rust bindings. Compiles for wasm32-wasip2 against `tvm-guest-mm`. Linked into `core::db`. |
| `sqlite-malloc-tvm` | Phase 2 sibling for malloc. Same shape. |
| (existing) `tvm-core` / `tvm-guest-mm` | Untouched. |

Both new crates live in this repo's workspace; they share the
sqlite headers we already vendor.

## Order of operations

1. **Validate the substrate**: build `tvm-guest-mm` against
   `wasm32-wasip2`, confirm the region API is usable from raw
   wasm32 with our toolchain (wasi-sdk). Estimate: a few hours.
   If `tvm-guest-mm` is wasm32-clean, proceed; if it assumes
   wasm64, raise an issue upstream first.
2. **Phase 1 — Path B**. The 80% solution.
3. **Measure**. If the residual 4 GB pressure is acceptable for
   the target workload, stop here.
4. **Phase 2 — Path C** if measurements demand it.
5. **Don't do Phase 3** unless A + B + C collectively fail.

## Out of scope

- Multi-threading. TVM's spec mentions handles crossing threads;
  our SQLite build is single-threaded today and that's not changing
  in this plan.
- Distributed page cache (cache shared across processes/hosts).
  Different problem; would need a TVM region whose backing is a
  network store, not a different SQLite integration.
- Encrypted regions. Same — orthogonal layer.
