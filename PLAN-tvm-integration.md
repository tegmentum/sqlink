# Plan: TVM integration + optional wasm64 builds (lifting the 4 GB wall)

Goal: support SQLite working sets — including page caches,
`:memory:` databases, intermediate query state — substantially
larger than 4 GB. Two parallel tracks:

1. **TVM track (default build)**: stay on wasm32, route SQLite's
   page cache and then its malloc layer through `tvm-guest-mm`.
   The destination is Path C (full TVM-managed allocator);
   Path B (TVM-backed pcache only) is the incremental on-ramp.
2. **Mem64 track (opt-in build)**: ship an alternate `wasm64-wasip2`
   build for users who want the address space without the TVM
   integration. Pay the ~10–20% perf hit for simpler operations.
   Buildable from the same source via a cargo feature.

This is a longer-horizon plan than the parity work in
`PLAN-rust-cli-parity.md`. It depends on `tvm-wasm` (`~/git/tvm-wasm/`),
the Tiered Virtual Memory substrate, and on wasmtime's memory64
support. Discrete from CLI work — its own document.

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

| | What it does | Cost | Role |
|---|---|---|---|
| **A. Status quo (wasivfs)** | File-backed I/O via host; page cache stays small | Zero | Always on |
| **B. Custom `pcache2`** | TVM-backed page cache via `sqlite3_config(SQLITE_CONFIG_PCACHE2, …)` | Medium | Incremental on-ramp to C |
| **C. Custom `malloc`** | Replace `sqlite3_malloc` family via `sqlite3_config(SQLITE_CONFIG_MALLOC, …)` | Higher | **Destination of the TVM track** |
| **D. wasm64 build** | Single 64-bit linear memory | ~10–20% perf, toolchain bootstrap | Optional opt-in alternate build target |

A is what we have today. B + C together are the TVM track; the
combination is the planned default build. D is a parallel,
opt-in build target — selectable via a cargo feature
(`mem64`), useful for callers who want > 4 GB without the TVM
integration surface.

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

## Phase 3 — optional wasm64 build target

A parallel track to B + C, not a replacement. Some callers want
the simpler operational story of a single 64-bit linear memory
and are willing to pay the perf hit. This phase ships a build
shape for them.

### Build configuration

A new cargo feature `mem64` on the `core`, `cli`, and
`sqlite-lib` crates. When enabled:

- Target switches from `wasm32-wasip2` to `wasm64-wasip2`.
- `libsqlite3-sys` rebuilds against the wasm64 sysroot
  (`CFLAGS_wasm64_wasip2`).
- Default linker/runtime settings switch to wasm64 equivalents.

Per-crate `.cargo/config.toml` carries the wasm64 env vars
under a `[env]` block guarded by the feature; the workspace-root
`.cargo/config.toml` adds `CC_wasm64_wasip2` / `AR_wasm64_wasip2`
/ `CFLAGS_wasm64_wasip2` to its `[env]` table so explicit builds
from the workspace root work too.

### Host-side support

`sqlite-wasm-host` adds `wasm_memory64(true)` to its wasmtime
`Config`. With the feature off, the engine rejects wasm64
modules (status quo). With on, it accepts both wasm32 and wasm64
components; the loaded extension can be either.

### Build matrix

After phases 1 + 2 + 3 land, the supported build configurations:

| Target | Pcache | Malloc | Max working set | Use case |
|---|---|---|---|---|
| wasm32-wasip2 | default pcache1 | default malloc | ~4 GB | Backward compat; small workloads |
| wasm32-wasip2 | tvm-pcache2 | default malloc | ~4 GB + tiered page cache | Phase 1 ship state |
| wasm32-wasip2 | tvm-pcache2 | tvm-malloc | Tiered, > 4 GB across the board | **Planned default** after phases 1 + 2 |
| wasm64-wasip2 | default pcache1 | default malloc | Whatever the host caps (typically 16 GB+) | `mem64` feature; users who want size without TVM |
| wasm64-wasip2 | tvm-pcache2 | tvm-malloc | Address space + tiering | Belt-and-braces; supported but rarely worth it |

### Cost

Less than B + C combined — it's mostly build-system plumbing.
The risk is in the `wasm64-wasip2` toolchain maturity (wasi-sdk,
cargo-component, wit-bindgen) and the test matrix doubling. No
SQLite code changes; no wasm component shape changes.

### Order

Phase 3 can land in parallel with B and C, **or** as the very
first thing if a user immediately needs > 4 GB and we haven't
shipped TVM integration yet. It doesn't gate B or C; B and C
don't gate it.

## What's published as crates

| Crate | Role |
|---|---|
| `sqlite-pcache-tvm` | C source for the Path B pcache2 impl + Rust bindings. Compiles for wasm32-wasip2 against `tvm-guest-mm`. Linked into `core::db`. |
| `sqlite-malloc-tvm` | Phase 2 sibling for malloc. Same shape. |
| (existing) `tvm-core` / `tvm-guest-mm` | Untouched. |

Both new crates live in this repo's workspace; they share the
sqlite headers we already vendor.

## Order of operations

The TVM track and the wasm64 track are independent. Within the
TVM track, B is the on-ramp to C; both are planned.

### TVM track

1. **Validate the substrate**: build `tvm-guest-mm` against
   `wasm32-wasip2`, confirm the region API is usable from raw
   wasm32 with our toolchain (wasi-sdk). Estimate: a few hours.
   If `tvm-guest-mm` is wasm32-clean, proceed; if it assumes
   wasm64, raise an issue upstream first.
2. **Phase 1 — Path B (pcache only)**. Lands first. Validates
   the integration shape, captures the 80% case, leaves
   `sqlite3_malloc` untouched.
3. **Measure**. Capacity test (> 4 GB hot working set) + perf
   regression budget.
4. **Phase 2 — Path C (full malloc)**. Adds the malloc replacement
   alongside the pcache. Same TVM region directory, new region
   types (`heap` + `arena`). After C lands, the TVM track's
   destination state is reached.

### Mem64 track (parallel)

Phase 3 — opt-in `wasm64-wasip2` build behind the `mem64`
cargo feature. Can land at any time relative to B/C; doesn't
gate or get gated by them. The TVM track stays the **default**
build configuration; `mem64` is opt-in for users who want the
address space without the integration surface.

## Out of scope

- Multi-threading. TVM's spec mentions handles crossing threads;
  our SQLite build is single-threaded today and that's not changing
  in this plan.
- Distributed page cache (cache shared across processes/hosts).
  Different problem; would need a TVM region whose backing is a
  network store, not a different SQLite integration.
- Encrypted regions. Same — orthogonal layer.
