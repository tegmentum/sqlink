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

## The five paths to lifting the wall

| | What it does | Cost | Role |
|---|---|---|---|
| **A. Status quo (wasivfs)** | File-backed I/O via host; page cache stays small | Zero | Always on |
| **B. Custom `pcache2`** | TVM-backed page cache via `sqlite3_config(SQLITE_CONFIG_PCACHE2, …)` | Medium | Lifts page-cache ceiling |
| **C. Custom `malloc`** | Replace `sqlite3_malloc` family via `sqlite3_config(SQLITE_CONFIG_MALLOC, …)` | Higher | In-proc backend only — TVM-backing structurally requires a SQLite fork, abandoned |
| **D. wasm64 build** | Single 64-bit linear memory | ~10–20% perf, toolchain bootstrap | Optional opt-in alternate build target, blocked on rustc + wasi-sdk |
| **E. TVM-backed custom VFS** | Per-file storage in TVM regions; sqlite treats them as files | Medium | **Destination of the TVM track for non-page bytes** |

A is what we have today. B + E together are the TVM track —
the combination covers everything SQLite stores: B holds the
hot page set, E holds the cold file data (database file, journal,
sort/hash spill, `temp_store=FILE` temp tables). C ships as the
in-proc allocator we already have; its TVM-backed variant was
considered (Phase 2.1) and rejected because SQLite returns raw C
pointers from `sqlite3_malloc` and dereferences them throughout
the codebase — TVM's non-default-memory regions can't satisfy
that contract without forking SQLite, which we won't do. D is a
parallel build target — selectable via the `mem64` cargo
feature — useful for callers who want > 4 GiB without the TVM
integration surface.

The key architectural insight that emerged during Phase 2: **the
"working set" that motivated this plan is mostly file-shaped
bytes**, not allocator-shaped bytes. Database pages, sort spill,
hash-join temps, in-memory tables — SQLite already routes all of
these through the VFS when configured with `temp_store=FILE`. The
allocator (`sqlite3_malloc`) holds only schema, prepared-statement
bytecode, lookaside, and result-row buffers, which are MB-scale
and never need > 4 GiB. So the > 4 GiB story is: B at the page
layer + E at the file layer, with `temp_store=FILE` opting users
into the file-layer story for transient bytes.

## How B, C, and E compose

Three disjoint subsystems, three different SQLite extension
points, three crates. They compose additively:

| Subsystem | Default impl | Replaced by |
|---|---|---|
| Page cache (largest single consumer in typical workloads) | `pcache1` (internally uses `sqlite3_malloc`) | `SQLITE_CONFIG_PCACHE2` (Path B) |
| General-purpose allocator: schema, statement bytecode, lookaside, result buffers | `sqlite3_malloc` (default `mem3`/`mem5`) | `SQLITE_CONFIG_MALLOC` (Path C) |
| File-shaped storage: db file, journal, WAL, sort/hash spill, in-memory dbs | `wasivfs` (in-proc/disk) or memdb VFS | `sqlite3_vfs_register(...)` (Path E) |

When you register a custom `pcache2`, your impl controls **its
own** allocations — it bypasses `sqlite3_malloc` for page
memory. When you register a custom VFS, the VFS controls its
own backing — `xRead/xWrite` move bytes between sqlite-supplied
buffers and your storage, with no allocator involvement on the
storage side. So:

- **B alone:** page cache lives in TVM. Everything else in
  default heap. > 4 GiB property held for hot pages only.
- **B + E:** page cache lives in TVM (via B), file content
  lives in TVM (via E). Big sorts spill through E because
  `temp_store=FILE` routes them through the VFS. **This is
  the destination state for the TVM track.** Schema +
  statement state stays in the C in-proc allocator, which is
  MB-scale and doesn't need lifting.
- **C alone (no B, no E):** default pcache1 sits on top of
  C's allocator. Doesn't break the 4 GiB ceiling because
  C's backing is the default heap.
- **B + C without E:** same > 4 GiB property as B alone for
  the page cache. C doesn't change the picture without E for
  file-shaped bytes.

**Why E rather than a TVM-backed C?** SQLite returns raw C
pointers from `sqlite3_malloc` and dereferences them inline,
with no callback boundary. Routing those bytes through TVM
would require either (a) keeping them in default memory anyway,
which defeats the > 4 GiB property TVM exists to provide, or
(b) forking SQLite to change the allocator contract, which is a
trust + maintenance cost we won't take. The VFS contract
already gives us the boundary — `xRead/xWrite` are exactly the
"copy bytes between caller's buffer and your storage" interface
TVM needs. The destination architecture uses the right layer
for each job.

## Architectural sketch

```
                ┌──────────────────────────────────┐
                │  sqlite3.c (unmodified upstream) │
                └─────────────┬────────────────────┘
                              │ uses
        ┌─────────────────────┼──────────────────────┐
        │                     │                      │
   pcache_methods2       mem_methods            sqlite3_vfs
   (CONFIG_PCACHE2)     (CONFIG_MALLOC)       (vfs_register)
        │                     │                      │
        ▼                     ▼                      ▼
  ┌──────────────┐    ┌────────────────┐    ┌────────────────┐
  │ sqlite-      │    │ sqlite-mem-tvm │    │ sqlite-vfs-tvm │
  │ pcache-tvm   │    │  (Path C, 2.0) │    │  (Path E, ph4) │
  │  (Path B)    │    │                │    │                │
  └──────┬───────┘    └────────────────┘    └────────┬───────┘
         │ shadow + flush                            │ xRead/xWrite
         ▼                                           ▼
   page-store regions                          general regions
         │                                           │
         └────────────────────┬──────────────────────┘
                              ▼
                  tvm:memory directory
                  (tvm-wasmtime host)

  sqlite-mem-tvm uses the Rust global allocator (default heap)
  — no TVM region involvement. The > 4 GiB story comes from
  pcache + VFS, the two layers where SQLite already provides
  the read/write boundary TVM needs.
```

Path B and Path E reduce to TVM region allocations against a
shared region directory. Region **types** differ: TVM's
`page-store` region carries fixed-size-slot semantics that fit
the pcache contract; the VFS uses general regions for
arbitrary-length file content. Path C stays out of the TVM
directory entirely — it's a pure Rust allocator over the global
heap, registered for the SQLite-facing API only.

## Phase 1 — Path B: TVM-backed `pcache2`

> **Phase 1.0 + 1.1 (in-process backend) shipped.** The
> `sqlite-pcache-tvm` crate implements the eleven
> `sqlite3_pcache_methods2` trampolines + Path D's shadow-pool +
> pinned-aware LRU + always-flush eviction against an in-process
> `Region` cold tier (`HashMap<u32, Vec<u8>>` keyed by
> `key * sz_page`). Two real-SQLite integration tests cover it:
> a baseline schema/select round-trip and a 1000-row workload
> with `PRAGMA cache_size = 4` that forces continuous eviction
> and cold-tier promotion. Six unit tests cover the
> shadow-pool + LRU + region invariants directly.
>
> **Phase 1.1 (TVM region backend) swap shipped.**
> `sqlite-pcache-tvm/src/wit_tvm_region.rs` implements
> `WitTvmRegion: Region` against the wit-bindgen-generated
> `tvm:memory/manager + bytes` interfaces. Gated on
> `target_arch = "wasm32"` — wasm builds always use the TVM
> backend, native builds get `InProcRegion` for the unit-test
> path (originally there was a separate `tvm` feature flag, but
> there's no realistic deployment where you'd want the in-proc
> backend on wasm  the feature was opt-in for no benefit, so
> it was dropped). The wasm32-wasip2 build verifies cleanly:
> `tvm:memory/manager`, `tvm:memory/bytes`, and
> `tvm:memory/diagnostics` imports show up in the rlib metadata,
> and the bundled SQLite compile via wasi-sdk in the same build
> links cleanly against the trampolines.
>
> **Phase 1.2 (end-to-end probe) shipped.**
> `probe/tvm-pcache-wasip2/` is a wasm32-wasip2 cdylib that
> links `sqlite-pcache-tvm` + sqlite-wasm-core
> (bundled libsqlite3-sys via wasi-sdk) into one ~1 MB
> component. The component imports
> `tvm:memory/{types,manager,bytes}` + WASI and exports
> `run-test`. The host-side test
> `host/tests/tvm_pcache_probe.rs` instantiates it against
> wasmtime + `tvm_wasmtime::add_to_linker`, calls `run-test`
> (which installs the pcache, opens an in-memory db, INSERTs
> 7 rows, SELECTs `count(*)`), and asserts `== 7`. Every page
> byte that misses the shadow pool round-trips through real
> `tvm:memory/bytes.read/write` host calls; the test passes,
> proving the wit-bindgen + TVM region + sqlite-pcache flow
> works end-to-end.
>
> **Phase 1.3 (capacity test) shipped.**
> `host/tests/tvm_pcache_capacity.rs` drives the probe's
> `run-capacity-test(50_000, 200)`  50K rows * 200 bytes
> through a 5-page (20 KiB) shadow pool against a file-backed
> SQLite db (wasivfs-mediated). The probe leaks the connection
> so SQLite's xDestroy doesn't tear the TVM region down before
> the host's assertion runs, then the host:
>
>   - confirms SQL integrity (count(*) returns 50000)
>   - asserts `WitTvmRegion::write` fired (we measured 55,368 calls)
>   - sums `region.used` across `TvmHost.directory.iter()` and
>     asserts >= 5 MiB (we measured 10 MB across 1 region)
>
> The architectural property the TVM track set out to prove is
> validated: the bulk of the working set lived in the TVM page-
> store region while the shadow pool stayed at ~5-6 pages in
> default wasm memory.
>
> Two real findings from the test build-out that warrant
> mentioning here:
>
> 1. **`:memory:` dbs use a non-purgeable pcache** that never
>    evicts pages  the cache contract says "you must keep
>    every page." File-backed dbs (which we drive through
>    wasivfs with a host-preopened tempdir) get the purgeable
>    cache where `xUnpin` becomes evictable, which is the path
>    Path D's design depends on.
>
> 2. **SQLite calls `xTruncate` aggressively** between
>    transactions (~501 times for the 500-transaction workload),
>    sometimes shrinking the cache to ~3 pages. This isn't a
>    bug  it's SQLite's pcache contract  but it means the
>    shadow pool oscillates near cap rather than growing
>    monotonically. Eviction still fires whenever the pool
>    refills above cap before the next truncate, which is
>    typical for SQLite's per-statement page touches.
>
> **What's left (Phase 2  Path C TVM-backed malloc) is its
> own design exercise.** The full sqlite3_malloc TVM swap
> would route ALL sqlite allocations (lookaside, schema, sort
> buffers, etc.) through TVM via SQLITE_CONFIG_MALLOC. Strictly
> additive  no rework of Phase 1.

> **Note on implementation language:** the plan called for "~500
> LOC C." Pure Rust shipped instead — `libsqlite3-sys` already
> binds the full `sqlite3_pcache_methods2` shape so the impl can
> be `extern "C" fn` trampolines pointing into Rust callbacks.
> Same ABI, no C glue to maintain.
>
> **Phase 1.1 architectural finding: shadow-pool indirection
> required.** SQLite's `pcache2.xFetch` returns a raw C pointer
> (`pBuf`) that SQLite dereferences directly when reading and
> writing page bytes. TVM's > 4 GiB story is fundamentally built
> on **non-default wasm memories** — each region is its own wasm
> memory addressed via the static `memory` immediate on
> `i32.load/store`. A C pointer into a non-default memory is not
> a valid pointer from the default memory's perspective, so a
> direct "the page slot lives in a TVM `page-store` region and
> we return its pointer" backend swap (what the original Phase 1
> sketch described) doesn't work.
>
> **The realistic shape (Path D — shadow-pool):**
>
> ```
>   sqlite3_pcache.xFetch(key, create) {
>       if (shadow_slot = lookup(key)) { return shadow_slot.pBuf; }
>       if (need_eviction()) {
>           victim = lru_evict();
>           tvm.bytes.write(victim.region_handle, victim.shadow_buf);  // flush back
>           free_shadow_slot(victim);
>       }
>       shadow = alloc_shadow_slot();        // bounded N-slot pool in default mem
>       region = lookup_or_alloc_region(key); // backed by TVM page-store, may be > 4 GiB
>       tvm.bytes.read(region, &shadow.buf); // pull page bytes into default mem
>       return shadow.pBuf;
>   }
>   sqlite3_pcache.xUnpin(page, discard) {
>       if (!discard) {
>           tvm.bytes.write(page.region_handle, page.shadow_buf); // flush
>       }
>       free_shadow_slot(page);
>   }
> ```
>
> Tradeoff: per-page-fault memcpy cost on cold fetches. Acceptable
> for SQLite because frequently-accessed pages stay pinned for the
> duration of an operation; eviction only fires when the working
> set genuinely exceeds the shadow pool. The shadow pool sizes
> the default-memory budget; the TVM region holds the cold tail.
>
> **Considered and rejected alternatives:**
>
> - **Path E (recompile SQLite for TVM dispatch).** Modify every
>   `pPage->aData[offset]` access in `sqlite3.c` to go through
>   TVM load/store helpers. Major surgery on a vendored SQLite —
>   breaks the "register a pcache2 impl" framing and ties us to a
>   forked SQLite. Out of scope.
> - **Path F (default-memory regions).** TVM as a sub-allocator
>   within the default linear memory. Gives handle abstraction +
>   eviction semantics but doesn't break 4 GiB. Useful for the
>   Phase 2 malloc layer (where the goal is allocator policy,
>   not address-space expansion); pointless for pcache.
>
> Phase 1.1 implements Path D against the WIT-bound
> `tvm:memory/manager + bytes` interfaces (the path validated by
> `probe/tvm-substrate/`).
>
> **Locked-in design decisions** (2026-06-14):
>
> - **Shadow-pool sizing: equal to `xCachesize`.** Matches the
>   existing `cache_size` PRAGMA semantics so operators reason
>   about the wasm-memory budget the same way they always have.
>   The TVM region grows beyond — pages above `xCachesize` live
>   cold in TVM and get faulted in on demand. Anyone who wants
>   more hot pages bumps `cache_size`, exactly as they would with
>   default pcache1. Rejected: "shadow = fraction of `xCachesize`"
>   adds a tuning knob no operator knows how to set and doesn't
>   compose with the existing PRAGMA expectations.
>
> - **Eviction policy: two-list LRU (pinned set + unpinned LRU).**
>   Pinned pages cannot be evicted (SQLite still holds the raw
>   `pBuf` pointer); `xUnpin` is the explicit "evictable now"
>   signal. Implementation: `HashMap<key, Entry>` for O(1)
>   lookup + intrusive linked-list pointers on `Entry` for O(1)
>   LRU promote / evict. Rejected: clock / approximate-LRU 
>   same hit rate on SQLite's typical temporal locality, more
>   code, no measurable win. Revisit only if profiling shows
>   pure LRU is the bottleneck.
>
> - **Dirty tracking: always flush on `xUnpin` unless
>   `discard=1`.** Phase 1.1 ships the conservative "doubles I/O
>   on read-only workloads" form. We have no in-band write
>   signal — SQLite writes through `pBuf` directly and the
>   pcache impl never sees the writes — so the alternatives are
>   either always-flush (Option A, locked in) or peek SQLite's
>   `PgHdr` dirty bit through `pExtra` (Option B, deferred).
>   Option B is brittle across SQLite versions (depends on the
>   PgHdr struct layout staying stable) and the win may be
>   smaller than expected: the flush cost is dominated by the
>   `tvm:memory.bytes.write` host call (wasm-host boundary
>   crossing), not the memcpy, so killing only-dirty flushes
>   reduces calls but not per-call latency. Promote to Option B
>   only after Phase 1.1 profiling shows the always-flush
>   overhead is the bottleneck on a representative workload
>   (TPC-H Q1 with shadow-pool sized to force eviction; target:
>   TVM-backed + always-flush within 20% of default pcache1).

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

> **Phase 2.0 (plumbing) shipped.** `sqlite-mem-tvm` crate
> ships the seven `sqlite3_mem_methods` trampolines + an
> `install()` registering via
> `sqlite3_config(SQLITE_CONFIG_MALLOC, …)` before
> `sqlite3_initialize`. Backend is a size-header allocator over
> the Rust global allocator: each allocation gets a 16-byte
> prefix holding the original requested size so `xSize` and
> `xFree` can recover it. Five unit tests cover the trampoline
> math (round-trip, realloc preserve, realloc-null = malloc,
> realloc-zero = free, xRoundup alignment). Two real-SQLite
> integration tests drive workloads through the trampolines and
> assert allocator counters climb (460 mallocs / 23 reallocs /
> 202 frees on a basic CREATE-INSERT-SELECT; group_concat over
> 1000 rows triggers reallocs as the accumulator grows).
>
> **Phase 2.1 (TVM-backed allocator) abandoned.** Two paths
> were considered and both rejected:
>
>   1. **Default-memory TVM regions exposing a base pointer.**
>      Gives sqlite a real C pointer to dereference but defeats
>      the > 4 GiB property — which is the only thing TVM
>      uniquely provides over the global allocator we already
>      use. Stripped of the > 4 GiB property, the wit-bindgen
>      host hop is pure overhead. No reason to use TVM at all.
>   2. **Forking SQLite to take handle-based allocator API.**
>      Trust cost is structural and permanent: SQLite's value
>      proposition is "this exact implementation with the
>      public test corpus." A fork loses that on day one,
>      forever, and anyone evaluating us would budget weeks of
>      audit time we can't pay. We won't fork.
>
> The misframe corrected during Phase 2 design: **the > 4 GiB
> story shouldn't come from the allocator at all.** SQLite's
> `sqlite3_malloc` holds schema, statement bytecode, lookaside,
> and result buffers — MB-scale, never needs to be lifted.
> The bytes that DO need > 4 GiB (page data, sort spill,
> temp tables, in-memory dbs) already route through the VFS
> when the right PRAGMAs are set. That's Phase 4's domain
> (TVM-backed VFS), and it doesn't need a fork.
>
> So Phase 2.0 is the malloc layer's end state on the TVM
> track. The `sqlite-mem-tvm` crate ships as-is. No follow-up.

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

> **No longer a destination state, kept as a future option.**
> Phase 3 was originally framed as the path that erases the
> 4 GiB question entirely when the toolchain catches up. With
> the TVM track now shipping unconditionally on wasm32 (B + E
> always-on, no feature flag), > 4 GiB working sets work today
> without waiting for wasm64. wasm64 stays interesting for the
> operational simplicity of a single 64-bit address space, but
> it's no longer a blocker for capacity.
>
> Host-side support stays: `Host::new` sets `wasm_memory64(true)`
> so the engine accepts wasm64 components when (and if) one
> shows up. The guest-side prerequisites are still unmet
> (`rustc` 1.96 doesn't ship `wasm64-wasip2`; wasi-sdk 33 has no
> wasm64 sysroot). The build-system plumbing lands once both
> upstreams catch up; until then, no one's blocked  the TVM
> track is the production > 4 GiB path.

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

## Phase 4 — Path E: TVM-backed custom VFS

> **Phase 4.0 + 4.1 + 4.2 shipped.** The `sqlite-vfs-tvm` crate
> ships the full sqlite3_vfs trampolines (4.0), feature-gated
> `WitTvmStorage` backend using 4 KB chunked allocs in
> `tvm:memory` regions (4.1), and the wasm32-wasip2 probe at
> `probe/tvm-vfs-wasip2/` with a host integration test that
> drives 100 INSERTs through real SQLite + the chunked TVM
> backend (4.2). The host test passes: TvmHost.directory
> reports 1 region holding 8 KB after the workload  bytes
> actually flowed through `tvm:memory/manager.create-region` +
> `bytes.write`, not through default memory.
>
> One small core::db change was needed to make the probe
> possible: `Connection::open_with_vfs(path, flags, vfs_name)`
> joined `Connection::open` so callers can name the VFS
> explicitly. The bare `Connection::open` hardcodes "wasivfs"
> for non-`:memory:` paths on wasm32, which would route past
> our just-installed VFS.

The path that picks up where Phase 2 dropped. With Phase 2.1
abandoned (allocator-layer TVM can't satisfy SQLite's
raw-pointer contract without forking), Phase 4 lifts the > 4 GiB
ceiling at a different layer: **the VFS.** Database files, journal
files, sort/hash spill, and `temp_store=FILE` temp tables all
flow through `sqlite3_vfs` — and the VFS's `xRead/xWrite`
contract is exactly the boundary the TVM allocator path
*didn't* have: bytes move between a caller-supplied buffer (in
default memory) and the VFS's storage (which can be anywhere).

### Why this works where Phase 2.1 didn't

Phase 1's pcache and Phase 2's mem methods both expose
`*mut c_void` to SQLite. SQLite then reads/writes those bytes
directly. Pcache made this tractable via the shadow-pool design
(pin window = fault into default memory; unpin = flush to TVM).
Mem methods had no such boundary.

The VFS has the boundary built in: SQLite never accesses VFS
storage by raw pointer. Every byte goes through `xRead(offset,
len, buf)` or `xWrite(offset, len, buf)`. The buffer is in
default memory (sqlite-supplied), but the storage can live in a
TVM region addressed by `(region_id, offset)`. No shadow pool
needed — sqlite already does the right thing structurally.

### What to build

`sqlite-vfs-tvm` crate, sibling to `sqlite-pcache-tvm` and
`sqlite-mem-tvm`. Implements `sqlite3_vfs` + `sqlite3_io_methods`
trampolines. Registers via `sqlite3_vfs_register` (NOT
`sqlite3_config` — VFS registration is a separate API and is
*not* boot-order-constrained the way `SQLITE_CONFIG_PCACHE2` is).

Per-file state holds:

- A `tvm:memory` region id (created at `xOpen` time)
- A `HashMap<u32, Handle>` mapping logical byte offset → TVM
  handle from `manager.alloc` (same shape as
  `WitTvmRegion::handles`)
- File size (updated by `xWrite` / `xTruncate`)
- File flags from the `xOpen` call

`xOpen` creates the region (sized to a reasonable initial
capacity); `xRead` looks up handles by offset and copies bytes
out via `tvm:memory/bytes.read`; `xWrite` allocates handles on
miss and copies bytes in via `bytes.write`; `xClose` destroys
the region.

Path resolution: the new VFS registers under a name (e.g.
`"tvm-mem"`); user code opens with `Connection::open_with_vfs(
"/tvm/big.db", "tvm-mem", ...)`. Alternatively a single
process-wide `tvm-mem` becomes the default VFS via
`sqlite3_vfs_register(..., make_default=1)`. Default-VFS is the
cleaner choice for users who want everything in TVM.

### Usage shape

```rust
// One-time setup:
sqlite_vfs_tvm::install()?;        // registers under name "tvm-mem"
// Or: sqlite_vfs_tvm::install_as_default()?; // becomes default VFS

// Open against the TVM VFS:
let conn = Connection::open_with_vfs("/big.db", "tvm-mem", FLAGS)?;
conn.execute_batch("PRAGMA temp_store = FILE;")?;  // route sort/hash through VFS
conn.execute_batch("PRAGMA cache_size = -8000;")?; // 8 MB shadow + rest in pcache region
// Now:
//   - DB file content lives in TVM (via this VFS)
//   - Page cache lives in TVM (via Phase 1 pcache)
//   - Sort/hash spill lives in TVM (PRAGMA + this VFS)
//   - Schema/bytecode lives in default heap (Phase 2.0)
// Result: > 4 GiB working sets, no disk I/O, no fork.
```

### Validation

- **Functional.** Open db via `tvm-mem` VFS, run a basic
  CREATE-INSERT-SELECT workload. Asserts data round-trips
  correctly. Mirrors `sqlite-pcache-tvm`'s
  `serves_real_sqlite.rs` test.
- **Spill-through-VFS.** Sort/hash spill goes through the
  registered VFS when `temp_store=FILE` is set. Trigger a
  workload that exceeds the in-memory sort threshold; assert
  the VFS region grew accordingly.
- **Capacity.** Same pattern as Phase 1.3: drive a large
  workload (rows × payload exceeding the shadow pool budget),
  inspect `TvmHost.directory` for region byte usage, assert
  TVM holds the bulk. Distinction from Phase 1.3: Phase 1.3
  proved pcache offload works; Phase 4 capacity proves VFS
  offload works for `:memory:`-style workloads where the
  whole "file" lives in TVM.

### Open questions for Phase 4

- **Region capacity strategy.** Pcache used a fixed default
  (256 MiB) because pages have predictable upper bounds tied
  to `xCachesize`. VFS files can grow arbitrarily. Options:
  start small + grow via additional region creations, or
  start with a large reserve. Defer until measurement shows
  which is cheaper at the host's TVM allocator.
- **Sync semantics.** SQLite's `xSync` exists to flush dirty
  data to durable storage. For TVM-backed files there's no
  durability — TVM regions are RAM-shaped. `xSync` is a
  no-op. Document this for users who expect crash safety.
- **Journal mode interaction.** Default rollback journal
  writes a separate file via VFS. If both main db and
  journal are in TVM, they're consistent (same lifetime).
  WAL adds a third VFS-managed file (the -shm). Both compose
  cleanly with our impl since the VFS sees them all uniformly.
- **Sharing TVM regions across connections.** Two
  `Connection::open` calls on the same path: should they share
  the underlying TVM region? sqlite expects file-sharing
  semantics (multi-reader, single-writer). For Phase 4.0 we
  ship per-connection regions (no sharing); the multi-conn
  story is Phase 4.1.

### What stays out of scope

- **Persistence.** TVM regions don't survive the wasm
  instance. For durable storage use `wasivfs` + host disk
  (already shipped). Phase 4 is for the "fast in-memory big
  working set" use case, not the "persistent db" use case.
- **Cross-instance shared TVM regions.** Same as above: each
  wasm instance gets its own TVM directory.
- **Compression / encryption** in the VFS layer. Orthogonal.

## What's published as crates

| Crate | Role |
|---|---|
| `sqlite-pcache-tvm` | Path B pcache2 impl (Phase 1, shipped). Rust trampolines + `ShadowCache<R: Region>` + in-proc and wit-bindgen-backed `Region` impls. |
| `sqlite-mem-tvm` | Path C mem methods impl (Phase 2.0, shipped). Size-header allocator over the Rust global allocator. No TVM-backed variant — see Phase 2 finding for why. |
| `sqlite-vfs-tvm` | Path E TVM-backed VFS. Phase 4.0 + 4.1 + 4.2 shipped: trampolines + InProcStorage + WitTvmStorage backend + wasm32-wasip2 probe with end-to-end SQLite round-trip through `tvm:memory` regions. |
| (external) `tvm-core` / `tvm-guest-mm` / `tvm-wasmtime` | Untouched; consumed via path deps. |

All three new crates live in this repo's workspace and share the
SQLite type surface via `libsqlite3-sys` (consumed without the
`bundled` feature — the bundled compile is in `core`).

## Order of operations

The TVM track and the wasm64 track are independent. Within the
TVM track, B + E together are the destination state. C is the
in-proc allocator we already ship.

### TVM track

1. **Validate the substrate** ✅. The wiring path is the
   WIT-bound `tvm:memory/{types,manager,bytes}` interfaces
   (not the raw-`extern "C"` path `tvm-guest-rt` exposes — that
   path is wasm32-unknown-unknown-only because the component
   model requires every guest import to be declared in WIT).
   Validation lives at `probe/tvm-substrate/` (wasm32-wasip2
   cdylib using wit-bindgen against `tvm:memory@0.1.0`) and
   `host/tests/tvm_substrate_probe.rs` (instantiates the
   component against `tvm_wasmtime::add_to_linker` + WASI,
   asserts the create-region → alloc → write → read → sum
   round-trip returns 10). Conclusion: the substrate is
   wasip2-clean via the WIT path; SQLite track Phase 1 can
   plug into the same wiring.
2. **Phase 1 — Path B (pcache)** ✅. Shipped through Phase 1.3.
   `sqlite-pcache-tvm` ships the shadow-pool + LRU cache with
   a wit-bindgen-backed `WitTvmRegion` cold tier. Capacity
   test (`host/tests/tvm_pcache_capacity.rs`) validates the
   architectural property end-to-end: 10 MB working set
   round-tripped through TVM while the shadow pool stayed at
   ~5-6 pages (20 KB).
3. **Phase 2 — Path C (mem methods)** ✅. Shipped at 2.0.
   `sqlite-mem-tvm` ships the size-header allocator over the
   Rust global heap. The TVM-backed variant (Phase 2.1) was
   considered and abandoned — see the Phase 2 finding for the
   reasoning. The malloc layer is done.
4. **Phase 4 — Path E (TVM-backed VFS)**. The next piece of
   work. Routes database file content + sort/hash spill +
   `:memory:`-style working sets through `sqlite3_vfs`
   trampolines whose backing is TVM regions. This is the
   layer where the > 4 GiB story actually closes for non-page
   bytes. Independent of Phase 2; depends only on the
   `tvm:memory` substrate (Phase 1's substrate validation
   covers it).
5. **Measure**. End-to-end capacity test against the combined
   B + E stack: open a `tvm-mem` VFS db with > 4 GiB of file
   content; assert wasm linear memory stays bounded; assert
   TVM region directory holds the bulk.

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
