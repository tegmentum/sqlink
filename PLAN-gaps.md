# Plan: gaps audit + sequencing

Comprehensive accounting of what's NOT in the codebase today,
grouped by category, with honest assessment of whether each gap
is worth filling.

This doc captures the audit from the dot-command parity check.
The sequencing section at the bottom names what we're shipping
next.

---

## 1. SQLite compile flags not enabled

Currently set (in `.cargo/config.toml`'s `LIBSQLITE3_FLAGS` +
`Makefile`'s `SQLITE_CFLAGS`):

  MATH_FUNCTIONS, COLUMN_METADATA, STAT4, SESSION,
  PREUPDATE_HOOK, GEOPOLY, DBSTAT_VTAB, STMTVTAB,
  BYTECODE_VTAB

Plus bundled defaults (FTS5, RTREE, JSON1, THREADSAFE).

| Flag | What it adds | Worth enabling? |
|---|---|---|
| `SQLITE_ENABLE_DESERIALIZE` | `sqlite3_deserialize` for in-memory dbs from blobs | YES  cheap (1 line + cli surface), useful for fixtures |
| `SQLITE_ENABLE_SNAPSHOT` | Point-in-time read snapshots over WAL | Defer  needs WAL first |
| `SQLITE_ENABLE_DBPAGE_VTAB` | Raw db page access vtab | Skip  `dbstat` covers most diagnostics; forensic-only otherwise |
| `SQLITE_ENABLE_FTS3 / FTS4` | Older FTS engines | Skip  FTS5 supersedes |
| `SQLITE_ENABLE_ICU` | Unicode collations + locale-aware fns | Skip  ~10 MB dep, huge for wasm |
| `SQLITE_ENABLE_NORMALIZE` | `sqlite3_normalized_sql()` | Useful for telemetry / query fingerprinting; defer |
| `SQLITE_ENABLE_OFFSET_SQL_FUNC` | `sqlite_offset()` | Niche; skip |
| `SQLITE_SOUNDEX` | `soundex()` builtin | Already covered by `text-nlp` ext; skip |

## 2. SQLite C API surface not exposed

The bundled SQLite has these symbols compiled in; our extension
WIT contract doesn't surface them:

| API | Use case | Cost to expose |
|---|---|---|
| `sqlite3_blob_open` / `_read` / `_write` / `_close` | Incremental BLOB I/O (don't load a 100 MB blob into memory) | New WIT `blob` interface; ~1 day |
| `sqlite3_deserialize` / `_serialize` | In-memory dbs from blobs | New WIT funcs + DESERIALIZE flag; ~3 hours |
| `sqlite3_backup_*` from extensions | Programmatic backup from ext code (already in cli via `.backup`) | SPI extension; ~3 hours |
| `sqlite3_progress_handler` | Long-query cancellation hook | New WIT callback shape; ~1 day |
| Authorizer toggle for non-authorizing extensions | Currently only authorizing world; no runtime on/off path | ~3 hours |

## 3. Architectural gaps (the dual-sqlite3 boundary)

| Gap | Severity |
|---|---|
| **WAL mode unsupported** — `PRAGMA journal_mode=WAL` silently falls back to `delete`. wasivfs lacks `.db-shm` (shared-memory) support. | HIGH  this is what every modern SQLite workload uses |
| **`:memory:` doesn't bridge host  wasm** — documented; every spi-dependent extension's smoke needs `-- smoke-db: tempfile` | Medium  documented workaround exists |
| **No transactional bridge** — host can't open a tx that the wasm cli sees, or vice versa | Medium  architectural; rarely needed |
| **No shared connection across extension instances** — each loaded ext gets its own host connection | Medium |

## 4. Wasm-component model gaps

| Gap | Severity |
|---|---|
| **No hot-reload** — load once, can't update without restarting cli | Medium  affects dev iteration |
| **No extension dependency declarations** — ext A can't require ext B at load time | Medium  manifest has no `requires:` field |
| **No resource isolation guarantees** — extension panic likely crashes cli; wasmtime SHOULD trap but unverified | HIGH |
| **No per-extension wasi preopens** — all share the cli's wasi context | Medium  security/multi-tenancy concern |
| **No profiling / per-extension call counts** | Low  nice-to-have |
| **No semver-aware loading** — load by file path, not by name+version | Medium  `define`/`semver` extensions are SQL-level only |
| **No stack traces from wasm panics** — error is `instantiate failed: ...` with no source line | HIGH  real dev pain point |

## 5. Distribution / tooling gaps

| Gap | Severity |
|---|---|
| **No top-level README** explaining "what is this project, who is it for" | HIGH  first contact with the repo is blind |
| **No signed extension repository / index** — CAS cache exists but no canonical fetch infrastructure | Medium  one-step-removed from `.load <url>` actually working |
| **No publish workflow** — `.load` works from local files; no `https://...` resolver wired to a real registry | Medium |
| **No reference verifier setup beyond test fixtures** | Low  the policy machinery exists |
| **No benchmarks** vs native sqlite3  claims like "vec0  sqlite-vec" are unverified | Medium |
| **No CHANGELOG** — git history is the only record | Low |

---

## Sequencing decision

Ranked by value × ease (cheapest high-value first):

| # | Item | Cost | Why this order |
|---|---|---|---|
| 1 | **Top-level README** | 2 hours | First contact for any new reader; cheapest high-value thing. Block first. |
| 2 | **`sqlite3_deserialize` support** | 3 hours | One compile flag + a cli command or scalar; opens fixture-from-blob workflows; cheap |
| 3 | **WAL support** | 3-5 days | Real vfs work in `core/src/vfs/`; transformative for modern workloads. Real undertaking; start after the cheap wins. |
| 4 | **Hot-reload + extension dependency declarations** | ~1-2 days each | Together they enable real iterative dev workflows. Bigger architectural surgery; do after WAL or in parallel if I run out of contiguous time. |

Everything else (#5+ in the original audit) genuinely needs a
concrete consumer to drive design  the lesson from the
daemon-vs-WIT-extension exercise. Defer until someone pulls.

## Execution log

This section tracks what's actually shipped from items 1-4.
Update after each commit.

- [x] 1. Top-level README  (db7d264)
- [x] 2. `sqlite3_deserialize` + `.serialize` / `.deserialize` dot cmds (3461f3c)
- [~] 3. WAL support  shm methods implemented in vfs_wasi.c (729a612)
       but UNREACHABLE. Upstream blocker found: sqlite3.c has
       `#if defined(__wasi__) ... #define SQLITE_OMIT_WAL 1`. The
       WAL code isn't compiled in at all on WASI targets. The shm
       implementation is ready for whenever we get past the source
       lockout (see below). Original 3-5 day estimate was for VFS
       work only  the actual blocker is upstream patching.
- [x] 4. Hot-reload  full workflow shipped.
       `.unload NAME; .load PATH` cycle (already implemented before
       this session). Added `.reload NAME [PATH]` shortcut for
       the edit-rebuild-reload dev loop (remembers the source from
       the last `.load`). Stress-tested across worlds: scalar
       (sha3), vtab (completion), collation (uint) all cycle
       correctly; 10x rapid reload no leak. Aggregate (hyperloglog,
       count_min, sketches) fails to load  PRE-EXISTING bug
       ("instantiate loaded ext: failed to convert function to
       given type"), unrelated to hot-reload, logged as
       follow-up.

       Extension dependency declarations  NOT done, intentionally
       deferred. Would require:
         - Adding a `requires-spec` record to the Manifest in
           sqlite-loader-wit/wit/guest.wit
         - Updating host's manifest dispatch path to surface the
           field through bindgen
         - Updating cli's `.load` flow to check requires against
           loaded set before registering
         - Manifest macro / scaffold template updates so authors
           can declare requires from extension Rust code
       Real WIT contract change. ~1-2 days. Deferred until a real
       multi-extension dependency case arises  the lesson from
       D/E architecture work was "don't ship speculative
       contracts."

## Item 4 follow-up: aggregate-extension load bug

Surfaced while stress-testing hot-reload across worlds. Loading
any extension that uses the `stateful` world for aggregates
fails with:

  Error loading <path>: instantiate loaded ext:
  failed to convert function to given type (code 1)

Affects: hyperloglog, count_min, sketches (verified). decimal /
stats build into workspace target paths so weren't tested here.

This is wasmtime-level type-mismatch on one of the aggregate
WIT functions  it predates this session's work and isn't a
hot-reload-specific issue. Worth investigating in its own pass
because the catalog claims aggregate support; ~half a day.

## Item 3 detail: WAL on WASI

What I did: implemented xShmMap, xShmLock, xShmBarrier, xShmUnmap
in `src/vfs/vfs_wasi.c`. The implementation is correct in shape:
in-memory shm with per-file refcounted regions; lock state as
trivial bookkeeping (single-threaded single-connection); no
.db-shm file written.

Why it doesn't work yet: the bundled sqlite3.c in libsqlite3-sys
unconditionally defines `SQLITE_OMIT_WAL` when `__wasi__` is set
(comment in source: "because it requires shared memory APIs").
That's a defensible upstream choice  most WASI runtimes can't
provide shm. Our wasivfs CAN, but the WAL code is compiled out
before our VFS gets to see anything.

Three paths forward, by cost:

| Path | Cost | Risk |
|---|---|---|
| A. Patch sqlite3.c via cargo build script (sed surgery before cc compile) | ~1-2 days | Fragile against libsqlite3-sys upgrades; cargo's bundled source path is not stable API |
| B. Vendor our own sqlite3.c, fork the WASI conditional | ~3-5 days | Maintenance burden; lose libsqlite3-sys's defaults; have to track upstream releases |
| C. Drop a custom #define before sqlite3.c is included (Makefile-based separate build) | ~1 week | Two parallel sqlite3 builds; complicates testing matrix |

Realistic recommendation: stay on path A but only commit to it
when we have a real WAL consumer. The shm implementation already
landed makes path A a 1-day surgery instead of a week of work.

For now, "read external WAL-mode db" remains BROKEN  this is the
real user-facing gap. Workaround: use `sqlite3` cli to
`PRAGMA journal_mode=DELETE` the external db before opening with
our cli.
