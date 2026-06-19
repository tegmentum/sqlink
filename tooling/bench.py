#!/usr/bin/env python3
"""Benchmark harness  run the same SQL workload through native
`sqlite3` and our `sqlite-wasm-run` cli, report timings and a ratio.

Each workload runs against a fresh on-disk db. Each measurement is
taken 5x and the median is reported (one-shot timings on a JIT'd
wasm runtime are too noisy to trust). Startup cost is amortized in
the workload itself  the bigger sizes show the steady-state cost
once instantiation + page cache prime are behind us.

Caveats:
  * SQLite versions differ slightly: native 3.43.x on this machine
    vs 3.53.2 in the wasm build (libsqlite3-sys 0.38.1). Expect
    minor planner differences.
  * The wasm runtime is wasmtime via sqlite-wasm-run; component
    instantiation + WIT-bindgen marshalling are on every call,
    so small workloads will skew "wasm is much slower"  the
    constant cost dominates. Read the larger sizes to see where
    the wasm  native ratio settles.
  * `sqlite3` (native) writes a SQLite-style summary on the last
    SELECT; we strip prompts the same way the smoke harness does.

Usage:
    tooling/bench.py            # run all benches
    tooling/bench.py --workloads insert,read
    tooling/bench.py --sizes 1000,10000,100000
"""

from __future__ import annotations

import argparse
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable

REPO_ROOT = Path(__file__).resolve().parent.parent
WASM_BIN = REPO_ROOT / "target" / "release" / "sqlite-wasm-run"
WASM_COMPONENT = REPO_ROOT / "target" / "wasm32-wasip2" / "release" / "sqlite_cli.component.wasm"
# Precompiled (AOT) variant; produced by `make precompile-cli`. Loads
# via Component::deserialize_file instead of Component::from_binary,
# saving ~360 ms of startup per invocation.
WASM_COMPONENT_CWASM = REPO_ROOT / "target" / "wasm32-wasip2" / "release" / "sqlite_cli.component.cwasm"
NATIVE_BIN = "sqlite3"
REPEATS = 5
DEFAULT_SIZES = [1_000, 10_000, 100_000]


@dataclass
class Engine:
    name: str
    runner: Callable[[str, str], float]


def _gen_insert_sql(n: int, journal_mode: str = "delete") -> str:
    """Bulk insert N rows in a single transaction. The cli wraps
    individual INSERTs in implicit transactions, which would dominate
    timing  use BEGIN/COMMIT explicitly to measure the engine, not
    the per-statement fsync."""
    lines = [
        f"PRAGMA journal_mode={journal_mode};",
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, val INTEGER);",
        "BEGIN;",
    ]
    for i in range(n):
        lines.append(f"INSERT INTO t VALUES({i}, 'row-{i}', {i * 7});")
    lines.append("COMMIT;")
    lines.append(f"SELECT count(*) FROM t;")
    return "\n".join(lines) + "\n"


def _gen_indexed_read_sql(n: int) -> str:
    """Build then read by primary key  measures B-tree lookup +
    page cache. N reads against the rows just inserted."""
    rows = [f"INSERT INTO t VALUES({i}, 'row-{i}', {i * 7});" for i in range(n)]
    selects = [f"SELECT name FROM t WHERE id={i % n};" for i in range(n)]
    return "\n".join([
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, val INTEGER);",
        "BEGIN;",
        *rows,
        "COMMIT;",
        *selects,
    ]) + "\n"


def _gen_aggregate_sql(n: int) -> str:
    """Full-scan aggregate. Measures the bytecode interpreter loop
    and column-extraction overhead, free of B-tree lookup cost."""
    rows = [f"INSERT INTO t VALUES({i}, 'row-{i}', {i * 7});" for i in range(n)]
    return "\n".join([
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, val INTEGER);",
        "BEGIN;",
        *rows,
        "COMMIT;",
        "SELECT count(*), sum(val), avg(val) FROM t;",
        "SELECT val % 10 AS g, count(*) FROM t GROUP BY g ORDER BY g;",
    ]) + "\n"


def _gen_join_sql(n: int) -> str:
    """N x N hash-join. Measures planner + temp-table allocation."""
    n_small = max(50, n // 100)
    rows_a = [f"INSERT INTO a VALUES({i}, {i % n_small});" for i in range(n)]
    rows_b = [f"INSERT INTO b VALUES({i}, 'b-{i}');" for i in range(n_small)]
    return "\n".join([
        "CREATE TABLE a(id INTEGER PRIMARY KEY, ref INTEGER);",
        "CREATE TABLE b(id INTEGER PRIMARY KEY, name TEXT);",
        "BEGIN;",
        *rows_a,
        *rows_b,
        "COMMIT;",
        "SELECT count(*) FROM a JOIN b ON a.ref = b.id;",
    ]) + "\n"


WORKLOADS = {
    "insert":   ("Bulk insert + count (single transaction)", _gen_insert_sql),
    "insert-wal": ("Bulk insert under WAL", lambda n: _gen_insert_sql(n, "wal")),
    "read":     ("PK index reads (N point lookups)", _gen_indexed_read_sql),
    "agg":      ("Full-scan aggregate + group-by", _gen_aggregate_sql),
    "join":     ("Hash join (N x N/100)", _gen_join_sql),
}


def time_native(db_path: str, sql: str) -> float:
    """Pipe SQL through native sqlite3. Returns wall-clock seconds."""
    t0 = time.perf_counter()
    subprocess.run(
        [NATIVE_BIN, db_path],
        input=sql,
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )
    return time.perf_counter() - t0


def time_wasm(db_path: str, sql: str, component: Path = WASM_COMPONENT) -> float:
    """Pipe SQL through sqlite-wasm-run. Returns wall-clock seconds.
    Component can be either the .wasm (parsed every invocation) or
    the .cwasm (precompiled, loaded via deserialize_file)."""
    cwd = os.path.dirname(db_path) or "."
    rel_db = os.path.basename(db_path)
    t0 = time.perf_counter()
    subprocess.run(
        [str(WASM_BIN), str(component), "--db", rel_db],
        input=sql,
        capture_output=True,
        text=True,
        timeout=600,
        cwd=cwd,
        check=False,
    )
    return time.perf_counter() - t0


def run_workload(
    workload: str, size: int, repeats: int = REPEATS,
    use_cwasm: bool = False,
) -> tuple[float, float]:
    """Return (native_median_s, wasm_median_s) for one workload+size.
    use_cwasm=True swaps in the precompiled component."""
    desc, gen = WORKLOADS[workload]
    sql = gen(size)
    component = WASM_COMPONENT_CWASM if use_cwasm else WASM_COMPONENT
    native_times: list[float] = []
    wasm_times: list[float] = []
    for _ in range(repeats):
        with tempfile.TemporaryDirectory(prefix="bench-native-") as d:
            db = os.path.join(d, "bench.db")
            native_times.append(time_native(db, sql))
        with tempfile.TemporaryDirectory(prefix="bench-wasm-") as d:
            db = os.path.join(d, "bench.db")
            wasm_times.append(time_wasm(db, sql, component))
    return statistics.median(native_times), statistics.median(wasm_times)


def fmt_secs(s: float) -> str:
    if s < 1.0:
        return f"{s*1000:.0f} ms"
    return f"{s:.2f} s"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--workloads", default=",".join(WORKLOADS.keys()),
                    help=f"Comma-separated workloads. Available: {', '.join(WORKLOADS.keys())}")
    ap.add_argument("--sizes", default=",".join(str(s) for s in DEFAULT_SIZES),
                    help="Comma-separated row counts (default: 1000,10000,100000)")
    ap.add_argument("--repeats", type=int, default=REPEATS,
                    help=f"Trials per measurement (default: {REPEATS}, median reported)")
    ap.add_argument("--markdown", action="store_true",
                    help="Emit results as a markdown table (for PLAN-benchmarks.md)")
    ap.add_argument("--cwasm", action="store_true",
                    help="Use the precompiled .cwasm component (run `make precompile-cli` first)")
    args = ap.parse_args()

    if not shutil.which(NATIVE_BIN):
        print(f"Error: {NATIVE_BIN} not found on PATH", file=sys.stderr)
        return 1
    if not WASM_BIN.exists():
        print(f"Error: {WASM_BIN} not built; run cargo build --release -p sqlite-wasm-host",
              file=sys.stderr)
        return 1
    if not WASM_COMPONENT.exists():
        print(f"Error: {WASM_COMPONENT} not built", file=sys.stderr)
        return 1
    if args.cwasm and not WASM_COMPONENT_CWASM.exists():
        print(f"Error: {WASM_COMPONENT_CWASM} not built; run `make precompile-cli`",
              file=sys.stderr)
        return 1

    workloads = [w.strip() for w in args.workloads.split(",") if w.strip()]
    sizes = [int(s.strip()) for s in args.sizes.split(",") if s.strip()]

    for w in workloads:
        if w not in WORKLOADS:
            print(f"Unknown workload: {w}; available: {', '.join(WORKLOADS.keys())}",
                  file=sys.stderr)
            return 2

    component_used = WASM_COMPONENT_CWASM if args.cwasm else WASM_COMPONENT
    print(f"\n  native: {NATIVE_BIN} ({subprocess.run([NATIVE_BIN, '--version'], capture_output=True, text=True).stdout.split()[0]})")
    print(f"  wasm:   {WASM_BIN.name} via {component_used.name}")
    print(f"  trials: {args.repeats}, median reported\n")

    if args.markdown:
        print("| Workload | Size | native | wasm | wasm/native |")
        print("|---|---:|---:|---:|---:|")
    else:
        print(f"  {'workload':<14} {'size':>8}  {'native':>9}  {'wasm':>9}  {'ratio':>6}")
        print(f"  {'-'*14} {'-'*8}  {'-'*9}  {'-'*9}  {'-'*6}")

    for w in workloads:
        desc, _ = WORKLOADS[w]
        for size in sizes:
            n_s, w_s = run_workload(w, size, args.repeats, args.cwasm)
            ratio = w_s / n_s if n_s > 0 else float("inf")
            if args.markdown:
                print(f"| `{w}` | {size:,} | {fmt_secs(n_s)} | {fmt_secs(w_s)} | {ratio:.1f}x |")
            else:
                print(f"  {w:<14} {size:>8,}  {fmt_secs(n_s):>9}  {fmt_secs(w_s):>9}  {ratio:>5.1f}x")

    return 0


if __name__ == "__main__":
    sys.exit(main())
