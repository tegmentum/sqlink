#!/usr/bin/env python3
"""Run an extension's smoke.sql against the cli + optional output assertions.

Each extension owns extensions/<name>/smoke.sql. This harness pipes
the file's statements through the cli and surfaces what comes back.

Failure modes detected:
  * panic / load error / missing scalar / instantiation failure
    (heuristic match on stdout+stderr)
  * if extensions/<name>/smoke.expected exists, the parsed cli
    output is diffed against the expected file. Mismatches FAIL.

smoke.expected format (one expected output per line, in SELECT order):
    plain text     exact match required
    ~~             skip this output (nondet / random / time-of-call)
    ?              any non-empty value accepted
    leading #      comment, ignored

If smoke.expected is absent the harness behaves as before (panic-only).

Usage:
    tooling/smoke.py <name>      # smoke one extension
    tooling/smoke.py --all       # smoke every ext that has a smoke.sql
    tooling/smoke.py --list      # list extensions with smoke.sql
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

CLI_BIN = REPO_ROOT / "target" / "release" / "sqlite-wasm-run"
CLI_COMPONENT = REPO_ROOT / "target" / "wasm32-wasip2" / "release" / "sqlite_cli.component.wasm"

# Strip leading prompts the cli prints: "sqlite> " and "   ...> ".
# Multiple can chain on one line when block comments are buffered.
PROMPT_RE = re.compile(r"^(sqlite>\s*|\s*\.\.\.>\s*)+")


def find_smoke_files() -> list[Path]:
    return sorted(REPO_ROOT.glob("extensions/*/smoke.sql"))


def parse_results(raw: str) -> list[str]:
    """Convert cli stdout into the ordered list of SELECT results.

    Strategy is intentionally simple: strip leading prompts from each
    line, skip blanks + the load banner, return what's left. The
    harness injects `.nullvalue <NULL>` before piping smoke.sql to
    the cli (see smoke_one()) so a NULL result renders as a literal
    `<NULL>` line  not blank  and survives the blank-skip below.
    Test files just write `<NULL>` in smoke.expected.
    """
    out: list[str] = []
    for line in raw.splitlines():
        stripped = PROMPT_RE.sub("", line).rstrip()
        if not stripped:
            continue
        if stripped.startswith("Loaded extension:"):
            continue
        out.append(stripped)
    return out


def parse_expected(path: Path) -> list[str]:
    """Parse smoke.expected. Comments + blank lines ignored.

    A comment line is `#` followed by whitespace (or EOL). A bare
    `#` glued to a value (e.g. `#ff8800`) is NOT a comment  the
    `color` extension's hex output collides with `#` otherwise.
    """
    out: list[str] = []
    for line in path.read_text().splitlines():
        s = line.rstrip()
        if not s.strip():
            continue
        ls = s.lstrip()
        if ls == "#" or ls.startswith("# "):
            continue
        out.append(s)
    return out


def count_smoke_selects(path: Path) -> int:
    """Static count of SELECT statements in smoke.sql.

    Used to detect smoke.expected staleness before running the cli.
    Strategy:
      1. strip block + line comments
      2. strip dot-commands (`.load`, `.headers`, etc.; they're
         newline-terminated, not semicolon-terminated, so they'd
         otherwise glue onto the next SELECT)
      3. split on `;`, count statements whose stripped content
         starts with `select` (case-insensitive)
    """
    text = path.read_text()
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.DOTALL)
    text = re.sub(r"--[^\n]*", " ", text)
    # Drop dot-command lines entirely  they terminate on newline.
    text = "\n".join(
        line for line in text.splitlines()
        if not line.lstrip().startswith(".")
    )
    count = 0
    for stmt in text.split(";"):
        s = stmt.strip()
        if s.lower().startswith("select"):
            count += 1
    return count


def staleness(name: str) -> str | None:
    """Return a one-line description if smoke.expected is stale or
    missing-and-not-needed. None means "either no smoke.expected or
    counts agree."
    """
    smoke = REPO_ROOT / "extensions" / name / "smoke.sql"
    expected = REPO_ROOT / "extensions" / name / "smoke.expected"
    if not smoke.exists() or not expected.exists():
        return None
    n_select = count_smoke_selects(smoke)
    n_expected = len(parse_expected(expected))
    if n_select != n_expected:
        return f"smoke.sql has {n_select} SELECT(s) but smoke.expected has {n_expected} row(s)"
    return None


def compare(actual: list[str], expected: list[str]) -> list[str]:
    """Return a list of mismatch descriptions (empty = match)."""
    diffs: list[str] = []
    if len(actual) != len(expected):
        diffs.append(
            f"length mismatch: actual={len(actual)} rows, expected={len(expected)}"
        )
    for i, (got, want) in enumerate(zip(actual, expected)):
        if want == "~~":
            continue
        if want == "?":
            if not got:
                diffs.append(f"row {i+1}: expected any non-empty value, got empty")
            continue
        if got != want:
            diffs.append(f"row {i+1}: expected {want!r}, got {got!r}")
    return diffs


def _detect_smoke_db_marker(raw_text: str) -> str | None:
    """T-40: detect `-- smoke-db: <value>` marker in smoke.sql's
    first 5 lines. Returns the value (e.g. 'tempfile' or a path)
    or None if no marker present.
    """
    for line in raw_text.splitlines()[:5]:
        s = line.strip()
        if s.startswith("-- smoke-db:"):
            return s.split(":", 1)[1].strip()
    return None


def _prepare_smoke(name: str) -> tuple[str, str | None] | str:
    """Read + transform smoke.sql for one extension. Returns:
        (sql_text, smoke_db_marker)  on success
        error_string                  if smoke.sql is missing
    """
    smoke = REPO_ROOT / "extensions" / name / "smoke.sql"
    if not smoke.exists():
        return f"no smoke.sql at {smoke.relative_to(REPO_ROOT)}"
    raw_text = smoke.read_text()
    marker = _detect_smoke_db_marker(raw_text)
    sql = "\n".join(
        line for line in raw_text.splitlines()
        if not line.lstrip().startswith("--")
    )
    sql = ".nullvalue <NULL>\n" + sql
    return (sql, marker)


def _build_argv(marker: str | None, no_cache: bool) -> tuple[list[str], str | None, str | None]:
    """Build the sqlite-wasm-run argv. Returns (argv, tmpdir, db_tempfile)
    where tmpdir / db_tempfile are paths to clean up (or None).
    """
    import tempfile
    argv = [str(CLI_BIN)]
    tmpdir = None
    db_tempfile = None
    if no_cache:
        tmpdir = tempfile.mkdtemp(prefix="sqlite-wasm-smoke-")
        argv += ["--cache-dir", tmpdir, "--no-component-cache"]
    # T-40: pick db path  marker overrides default.
    if marker == "tempfile":
        fd, db_path = tempfile.mkstemp(prefix="sqlite-wasm-smoke-", suffix=".db")
        os.close(fd)
        os.unlink(db_path)  # remove empty file; sqlite creates it on open
        db_tempfile = db_path
        argv += [str(CLI_COMPONENT), "--db", db_path]
    elif marker:
        argv += [str(CLI_COMPONENT), "--db", marker]
    else:
        argv += [str(CLI_COMPONENT), "--db", ":memory:"]
    return argv, tmpdir, db_tempfile


def _cleanup(tmpdir: str | None, db_tempfile: str | None) -> None:
    if tmpdir:
        import shutil
        shutil.rmtree(tmpdir, ignore_errors=True)
    if db_tempfile and os.path.exists(db_tempfile):
        os.unlink(db_tempfile)


def smoke_one(name: str, timeout: int = 30, no_cache: bool = False) -> tuple[bool, str]:
    if not CLI_BIN.exists():
        return (False, f"cli runner not built: {CLI_BIN.relative_to(REPO_ROOT)} missing; "
                       f"run: cargo build --release -p sqlite-wasm-host")
    if not CLI_COMPONENT.exists():
        return (False, f"cli component not built: {CLI_COMPONENT.relative_to(REPO_ROOT)} missing")

    prep = _prepare_smoke(name)
    if isinstance(prep, str):
        return (False, prep)
    sql, marker = prep

    argv, tmpdir, db_tempfile = _build_argv(marker, no_cache)
    try:
        result = subprocess.run(
            argv,
            input=sql,
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=REPO_ROOT,
        )
    except subprocess.TimeoutExpired:
        return (False, f"timeout after {timeout}s")
    finally:
        _cleanup(tmpdir, db_tempfile)

    out = result.stdout + result.stderr

    # First-pass: panic / load failure heuristic.
    panic_markers = (
        "Error loading",
        "no such function",
        "panicked",
        "instantiate loaded ext",
    )
    if any(m in out for m in panic_markers):
        return (False, out)

    # Pre-check: smoke.expected staleness  one-line warning if the
    # SELECT count in smoke.sql doesn't match the row count in
    # smoke.expected. Doesn't fail by itself; the actual diff will
    # catch the mismatch concretely.
    if (stale := staleness(name)):
        out = f"WARN: {stale}\n{out}"

    # T-11: warn if NO smoke.expected exists AND every parsed row is
    # <NULL>. That's the "fresh extension where every scalar silently
    # NULLs everything" signature  catches a fat typo class before
    # smoke.expected gets seeded. Skip the warn when smoke.expected
    # is present (the real diff below catches it concretely).
    expected_path = REPO_ROOT / "extensions" / name / "smoke.expected"
    if not expected_path.exists():
        actual = parse_results(result.stdout)
        if len(actual) >= 5 and all(row == "<NULL>" for row in actual):
            out = ("WARN: every parsed row is <NULL>  is your scalar "
                   "implementation wired up? (no smoke.expected yet to "
                   "diff against; this heuristic suppresses once you "
                   "seed one.)\n" + out)

    # Second-pass (optional): assert outputs against smoke.expected.
    if expected_path.exists():
        actual = parse_results(result.stdout)
        expected = parse_expected(expected_path)
        diffs = compare(actual, expected)
        if diffs:
            msg = ["output mismatch vs smoke.expected:"]
            msg.extend(f"  {d}" for d in diffs)
            msg.append("--- parsed actual ---")
            msg.extend(f"  {i+1}: {row}" for i, row in enumerate(actual))
            return (False, "\n".join(msg))

    return (True, out)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("name", nargs="?", help="extension to smoke")
    p.add_argument("--all", action="store_true", help="smoke every extension that has smoke.sql")
    p.add_argument("--list", action="store_true", help="list extensions with smoke.sql")
    p.add_argument("--timeout", type=int, default=30)
    p.add_argument("-j", "--jobs", type=int, default=1,
                   help="parallel workers for --all (0 = cpu_count). "
                        "Each smoke is independent; ~85 exts × 4s serial "
                        "vs <60s with -j 0.")
    p.add_argument("--show-parsed", metavar="NAME",
                   help="print the parsed result rows for one extension and exit "
                        "(useful when seeding smoke.expected)")
    p.add_argument("--seed-expected", metavar="NAME",
                   help="write smoke.expected for NAME from the current cli "
                        "output. Refuses to overwrite an existing file  "
                        "delete it first if reseeding intentionally.")
    args = p.parse_args()
    if not (args.name or args.all or args.list or args.show_parsed or args.seed_expected):
        p.error("specify <name>, --all, --list, --show-parsed, or --seed-expected")

    if args.seed_expected:
        name = args.seed_expected
        expected = REPO_ROOT / "extensions" / name / "smoke.expected"
        if expected.exists():
            print(f"smoke.expected already exists at {expected.relative_to(REPO_ROOT)}",
                  file=sys.stderr)
            print("delete it first if you intend to reseed.", file=sys.stderr)
            sys.exit(1)
        prep = _prepare_smoke(name)
        if isinstance(prep, str):
            print(prep, file=sys.stderr)
            sys.exit(1)
        sql, marker = prep
        argv, tmpdir, db_tempfile = _build_argv(marker, False)
        try:
            r = subprocess.run(
                argv, input=sql, capture_output=True, text=True, timeout=args.timeout,
            )
        finally:
            _cleanup(tmpdir, db_tempfile)
        rows = parse_results(r.stdout)
        header = (
            "# AUTO-SEEDED by smoke.py --seed-expected. Review and trim:\n"
            "#   - replace nondeterministic rows (timestamps, rng) with ~~\n"
            "#   - replace order-sensitive rows with ? if any-non-empty is OK\n"
            "#   - delete this banner once you've reviewed each row\n"
        )
        expected.write_text(header + "\n".join(rows) + "\n")
        print(f"wrote {len(rows)} rows to {expected.relative_to(REPO_ROOT)}")
        return

    if args.show_parsed:
        # smoke_one's stdout isn't exposed; re-run via the shared
        # helpers so the smoke-db marker is honored too.
        prep = _prepare_smoke(args.show_parsed)
        if isinstance(prep, str):
            print(prep, file=sys.stderr)
            sys.exit(1)
        sql, marker = prep
        argv, tmpdir, db_tempfile = _build_argv(marker, False)
        try:
            r = subprocess.run(
                argv, input=sql, capture_output=True, text=True, timeout=args.timeout,
            )
        finally:
            _cleanup(tmpdir, db_tempfile)
        for row in parse_results(r.stdout):
            print(row)
        return

    if args.list:
        for f in find_smoke_files():
            has_expected = (f.parent / "smoke.expected").exists()
            stale = staleness(f.parent.name) if has_expected else None
            marker = ""
            if has_expected:
                marker = " [asserted, STALE]" if stale else " [asserted]"
            line = f"{f.parent.name}{marker}"
            if stale:
                line += f"  {stale}"
            print(line)
        return

    targets: list[str]
    if args.all:
        targets = [f.parent.name for f in find_smoke_files()]
    else:
        targets = [args.name]

    fails: list[str] = []
    if args.jobs == 1 or len(targets) == 1:
        # Serial path  preserved output ordering, easiest to read in
        # logs. Default for single-name invocations.
        for name in targets:
            ok, output = smoke_one(name, args.timeout)
            status = "PASS" if ok else "FAIL"
            print(f"{status}  {name}")
            if not ok:
                fails.append(name)
                for line in output.split("\n")[:30]:
                    print(f"    {line}")
    else:
        # T-17: parallel fan-out for --all. Each smoke is an independent
        # subprocess  no shared state, no I/O contention besides the
        # cli binary (read-only). Threads, not processes  the work is
        # subprocess-bound (smoke_one waits on subprocess.run), so GIL
        # release during I/O is enough; ProcessPoolExecutor would
        # pay fork/import cost per worker for no win.
        import concurrent.futures
        import os
        workers = args.jobs if args.jobs > 0 else (os.cpu_count() or 4)
        with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as ex:
            futures = {ex.submit(smoke_one, name, args.timeout, True): name
                       for name in targets}
            for fut in concurrent.futures.as_completed(futures):
                name = futures[fut]
                ok, output = fut.result()
                status = "PASS" if ok else "FAIL"
                print(f"{status}  {name}")
                if not ok:
                    fails.append(name)
                    for line in output.split("\n")[:30]:
                        print(f"    {line}")

    if fails:
        print(f"\n{len(fails)} failed: {', '.join(fails)}")
        sys.exit(1)
    print(f"\nall {len(targets)} passed")


if __name__ == "__main__":
    main()
