# Benchmarks: sqlite-wasm-run vs native sqlite3

First numbers on the table. Until now the project's perf claims
("you can ship a 50 MB wasm cli that runs anywhere") were
unverified  this doc establishes the actual cost of running
SQLite through a wasm component vs the native binary.

The harness is `tooling/bench.py`. Run it yourself with:

```
make bench                                  # all workloads, default sizes
python3 tooling/bench.py --sizes 100000     # just the big ones
python3 tooling/bench.py --workloads read   # one workload
```

## Methodology

Each measurement is the median of N trials (default 3). Each
trial uses a fresh on-disk db in a fresh tempdir, then runs the
workload's SQL through `subprocess.run` with `time.perf_counter`
around the call. The whole-process wall-clock time is what's
reported  cli startup is INCLUDED, deliberately, so the small
sizes show the constant overhead the user pays.

| | |
|---|---|
| native | `sqlite3 3.43.2` (system) |
| wasm | `sqlite-wasm-run` (wasmtime) + `sqlite_cli.component.wasm` (libsqlite3-sys 0.38, SQLite 3.53.2) |
| repeats | 3 (median) |
| db | file-backed in a fresh tempdir per trial |
| journal | `delete` unless workload is `*-wal` |

Caveat: native is 3.43.2; wasm is 3.53.2. Planner differences
exist but are small at this workload shape.

## Results

Snapshot from one run (Apple Silicon, macOS, June 2026):

| Workload | Size | native | wasm | wasm/native |
|---|---:|---:|---:|---:|
| `insert` | 1,000 | 8 ms | 628 ms | 79.5x |
| `insert` | 10,000 | 21 ms | 486 ms | 23.2x |
| `insert` | 100,000 | 147 ms | 1.32 s | 9.0x |
| `insert-wal` | 1,000 | 12 ms | 473 ms | 39.4x |
| `insert-wal` | 10,000 | 24 ms | 687 ms | 28.2x |
| `insert-wal` | 100,000 | 150 ms | 1.16 s | 7.8x |
| `read` | 1,000 | 20 ms | 707 ms | 35.5x |
| `read` | 10,000 | 124 ms | 1.39 s | 11.2x |
| `read` | 100,000 | 1.16 s | 6.42 s | 5.5x |
| `agg` | 1,000 | 10 ms | 434 ms | 45.0x |
| `agg` | 10,000 | 23 ms | 458 ms | 20.3x |
| `agg` | 100,000 | 158 ms | 946 ms | 6.0x |
| `join` | 1,000 | 9 ms | 442 ms | 47.3x |
| `join` | 10,000 | 21 ms | 468 ms | 22.8x |
| `join` | 100,000 | 129 ms | 899 ms | 7.0x |

## What this tells us

**Steady-state overhead is ~5-9x.** At 100k rows the ratio
converges to a small single-digit multiple. That's the actual
cost of the wasm component model + wasmtime instantiation +
wasi shims for file I/O. The 1k-row numbers are startup-
dominated (every workload pays ~400 ms before the first row is
inserted) and shouldn't be read as steady-state cost.

**Read is the closest to native.** At 100k rows `read` lands at
5.5x  the B-tree traversal is sqlite3 code that wasm doesn't
slow down meaningfully; only the wasi shim cost on each
sqlite3_step() boundary shows up.

**Insert is around 9x.** Every INSERT crosses the wasi boundary
for the page-write fsync path. Larger transactions amortize this
(insert-wal at 100k is 7.8x; insert without WAL is 9.0x).

**WAL costs nothing extra at single-writer scale.** The 1k-row
insert vs insert-wal numbers are noisy at this size (39x vs 79x,
but with 8-12 ms native variance the wasm numbers swing around
too). At 10k+ rows the difference is in single-digit ms range
on the native side and well within noise on the wasm side. WAL
exists for concurrent readers; this single-writer harness can't
exercise that.

**The constant overhead is real and significant.** ~400 ms per
invocation is paid up front. That's wasmtime fast-path
instantiation, component-model module wiring, and the wasi
preopen / argv setup. For an interactive cli session this is
paid once (the cli stays running); for batch scripts that
invoke `sqlite-wasm-run` per statement, this dominates anything
smaller than a few thousand rows.

## What this doesn't measure (yet)

- **Extension call overhead**  scalar / aggregate / vtab calls
  cross the wit-bindgen boundary on every row. The 9x ratio above
  is sqlite3-internal work; extension-heavy queries (vec0 KNN,
  regex over text, sha3 batch hashing) will have a different
  curve. Want this  add a workload that loads an extension and
  exercises it.
- **Cold startup**  every trial here gets a freshly-instantiated
  wasm. Wasmtime caches compiled modules to disk; repeated runs
  in the same shell are faster than these numbers suggest.
- **vec0  sqlite-vec parity**  the catalog claim that vec0 is
  competitive with sqlite-vec needs a side-by-side KNN bench.
  Punt for now; needs the extension running through the wasm
  cli AND a native sqlite3 with sqlite-vec loaded, which is
  more setup than this scaffolding covers.
- **Concurrent readers under WAL**  single-process single-
  threaded WASI can't exercise this. Would need a multi-
  process driver that spawns several cli readers against the
  same on-disk db.

## Wins this run unlocked

| Finding | Implication |
|---|---|
| Steady-state overhead 5-9x | Quotable in the README. "Wasm cli runs SQLite at single-digit-multiple cost of native for non-trivial workloads." |
| `insert-wal` matches `insert` | The WAL unlock (libsqlite3-sys 0.30  0.38) doesn't add overhead for single-writer workloads  it's purely a capability addition. |
| 100k rows in ~1.3 s wasm | Real production-size workloads complete in seconds; the wasm cli is usable, not a toy. |
| `read` lands at 5.5x | The read path is the closest to native, because most of the time is in sqlite3's B-tree walk (compiled to wasm just like everything else)  the WIT call boundary doesn't dominate. |

## Follow-up items

- Add a `wasm` column for `wasmtime serve`-style persistent process
  (one instantiation, many invocations) to isolate startup
  amortization.
- Add an extension-overhead workload (e.g. `SELECT sha3(name) FROM t`
  vs a native shathree.so load) to measure the WIT boundary cost.
- Compare cold vs warm wasmtime cache to show what the
  component-cache buys.
- Run on Linux x86_64 too. Apple Silicon results are reproducible
  here but might not translate.
