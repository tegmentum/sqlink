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
- [x] 3. WAL support  shipped. Upstream SQLite 3.46  3.53.2
       (libsqlite3-sys 0.30  0.38) removed the WASI OMIT_WAL
       defeat. Bumped libsqlite3-sys to 0.38 across all callers
       (core, host, cli, sqlite-lib, sqlite-mem-tvm,
       sqlite-vfs-tvm, sqlite-pcache-tvm). The shm hooks already
       in vfs_wasi.c (729a612) became reachable; PRAGMA
       journal_mode=WAL now succeeds, writes go through, and
       wal_checkpoint(TRUNCATE) cleans up the .db-wal file.
       cli-smokes/wal.{sql,expected} guards the regression.
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

## Item 4 follow-up: aggregate-extension load bug  RESOLVED

Surfaced while stress-testing hot-reload across worlds. Loading
any extension that uses the `stateful` world for aggregates
failed with:

  Error loading <path>: instantiate loaded ext:
  failed to convert function to given type (code 1)

Affected: hyperloglog, count_min, sketches, decimal, stats  all
5 stateful-world extensions.

Root cause: stale component artifacts. Commit a25be4d
(Jun 16 2026) added `capability::dns` to `policy.capability`,
changing the variant from 11 to 12 cases. The 5 affected
extensions were last touched Jun 17 in b2dd18b (cute-name
rename) which didn't trigger a rebuild. The on-disk components
still carried the 11-variant variant; the host bindgen now
expects 12 cases. wasmtime's component instantiation conversion
walks every type in every export's signature  `metadata.describe`
returns a Manifest with `list<capability>`, so the variant
shape was on the conversion hot path  instantiation rejected.

Fix: rebuild the 5 extensions against current WIT. All 5 load
cleanly post-rebuild and aggregate end-to-end as designed
(hll cardinality, CMS estimate, t-digest quantile, decimal_sum,
regr_r2 all verified). Wrote smoke.sql + smoke.expected for
each  the regression class (stale build vs evolving WIT) was
silent specifically because these 5 had no smoke tests. With
smokes in place a future variant change that drops the rebuild
is now an explicit FAIL in `make ext-smoke-all`.

Total catalog smokes: 40  45.

## Item 3 detail: WAL on WASI  RESOLVED

What I did first: implemented xShmMap, xShmLock, xShmBarrier,
xShmUnmap in `src/vfs/vfs_wasi.c` (729a612). The implementation
is correct in shape: in-memory shm with per-file refcounted
regions; lock state as trivial bookkeeping (single-threaded
single-connection); no .db-shm file written.

Why it didn't work at first: the bundled sqlite3.c in
libsqlite3-sys 0.30 (SQLite 3.46.0) unconditionally defined
`SQLITE_OMIT_WAL` when `__wasi__` was set, with the explicit
comment "because it requires shared memory APIs." That made
the WAL subsystem invisible to our VFS even with shm hooks
present.

What I missed in the original audit: upstream SQLite 3.53.2
dropped that conditional. The `__wasi__` block at the top of
sqlite3.c now only defines OMIT_LOAD_EXTENSION and zeros
THREADSAFE  the WAL OMIT survives only in the SQLITE_OS_KV
branch (which we don't use). libsqlite3-sys 0.38.1 bundles
3.53.2, so the fix is just a version bump.

What I shipped: bumped libsqlite3-sys 0.30  0.38 across all
seven callers (core, host, cli, sqlite-lib, sqlite-mem-tvm,
sqlite-vfs-tvm, sqlite-pcache-tvm). One change of `bundled`
on host  `default-features = false` to satisfy cargo's
links-uniqueness rule. WAL now works end to end. The original
1-2 day surgery estimate was for sed-patching libsqlite3-sys's
bundled source, which is no longer needed.

Total: an hour, mostly cargo's `links =` conflict triage.
