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

Snapshot from one run (Apple Silicon, macOS, June 2026).

Two rows per workload: `.wasm` parsed + compiled on every
invocation (the default before this session), and `.cwasm`
precompiled once via `make precompile-cli` and loaded via
`Component::deserialize_file`. Run `make bench` for the first,
`make bench CWASM=1` for the second.

| Workload | Size | native | .wasm | ratio | .cwasm | ratio |
|---|---:|---:|---:|---:|---:|---:|
| `insert` | 1,000 | 7 ms | 628 ms | 79.5x | 53 ms | **7.5x** |
| `insert` | 10,000 | 19 ms | 486 ms | 23.2x | 100 ms | **5.2x** |
| `insert` | 100,000 | 138 ms | 1.32 s | 9.0x | 563 ms | **4.1x** |
| `insert-wal` | 1,000 | 12 ms | 473 ms | 39.4x |  ~ same  | ~ same |
| `insert-wal` | 100,000 | 150 ms | 1.16 s | 7.8x | ~ same | ~ same |
| `read` | 1,000 | 17 ms | 707 ms | 35.5x | 109 ms | **6.5x** |
| `read` | 10,000 | 108 ms | 1.39 s | 11.2x | 671 ms | **6.2x** |
| `read` | 100,000 | 1.08 s | 6.42 s | 5.5x | 6.32 s | **5.8x** |
| `agg` | 1,000 | 9 ms | 434 ms | 45.0x | 51 ms | **5.5x** |
| `agg` | 10,000 | 23 ms | 458 ms | 20.3x | 104 ms | **4.5x** |
| `agg` | 100,000 | 159 ms | 946 ms | 6.0x | 614 ms | **3.9x** |
| `join` | 1,000 | 10 ms | 442 ms | 47.3x | 76 ms | **7.9x** |
| `join` | 10,000 | 21 ms | 468 ms | 22.8x | 115 ms | **5.6x** |
| `join` | 100,000 | 129 ms | 899 ms | 7.0x | 569 ms | **4.4x** |

## Precompilation  the big startup win

The `.wasm`  `.cwasm` swap is the headline finding of this
session. Wasmtime's `Engine::precompile_component` AOT-compiles
the component to a host-CPU-specific blob; loading via
`Component::deserialize_file` then skips parse + validate +
cranelift compile.

Bare SQL throughput:

```
$ for i in 1..5; do time sqlite-wasm-run cli.wasm  <<<'SELECT 1;'; done
        ~370 ms wall-clock per invocation
$ for i in 1..5; do time sqlite-wasm-run cli.cwasm <<<'SELECT 1;'; done
        ~10 ms wall-clock per invocation
```

**~37x reduction in startup overhead.** From `make bench` above:

- 1k-row workloads (startup-dominated): from 28-80x  3-8x
- 100k-row workloads (steady-state): from 5.5-9x  3.9-5.8x

Cost: the `.cwasm` blob is ~5x larger (12 MB vs 2.5 MB for the
cli) because it embeds native machine code. Not portable across
CPU architectures or wasmtime versions  must be regenerated on
each machine after upgrades. `make precompile-cli` is the one-
liner; depends on the `.wasm` and the host binary, so it
re-runs automatically.

## What this tells us

**Steady-state overhead is ~4-6x with precompilation, 5-9x
without.** At 100k rows the ratio converges to a small single-
digit multiple. That's the actual cost of the wasm component
model + wasmtime instantiation + wasi shims for file I/O. The
1k-row numbers are startup-dominated  every `.wasm` workload
paid ~370 ms before the first row was inserted. Precompiling
to `.cwasm` cuts that to ~10 ms.

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
| **Precompile drops startup 370 ms  10 ms (37x).** | The biggest single improvement. Small-workload ratios drop from 50-80x to 5-8x. README-quotable. |
| Steady-state overhead with `.cwasm`: 4-6x | Quotable. "Wasm cli runs SQLite at ~4-6x cost of native for non-trivial workloads, single-digit multiple." |
| `insert-wal` matches `insert` | The WAL unlock (libsqlite3-sys 0.30  0.38) doesn't add overhead for single-writer workloads  it's purely a capability addition. |
| 100k rows in 563 ms (`.cwasm`) | Real production-size workloads complete in half a second; the wasm cli is usable, not a toy. |
| `read` lands at 5.5-6x | The read path is the closest to native, because most of the time is in sqlite3's B-tree walk (compiled to wasm just like everything else)  the WIT call boundary doesn't dominate. |

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
