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
    """Parse smoke.expected. Comments + blank lines ignored."""
    out: list[str] = []
    for line in path.read_text().splitlines():
        s = line.rstrip()
        if not s.strip():
            continue
        if s.lstrip().startswith("#"):
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


def smoke_one(name: str, timeout: int = 30) -> tuple[bool, str]:
    smoke = REPO_ROOT / "extensions" / name / "smoke.sql"
    if not smoke.exists():
        return (False, f"no smoke.sql at {smoke.relative_to(REPO_ROOT)}")
    if not CLI_BIN.exists():
        return (False, f"cli runner not built: {CLI_BIN.relative_to(REPO_ROOT)} missing; "
                       f"run: cargo build --release -p sqlite-wasm-host")
    if not CLI_COMPONENT.exists():
        return (False, f"cli component not built: {CLI_COMPONENT.relative_to(REPO_ROOT)} missing")

    # Strip `--` line comments. The cli's parser fuses leading
    # `--` comments with the following dot-command and chokes
    # on `.load`. See lessons-learned T-9.
    sql = "\n".join(
        line for line in smoke.read_text().splitlines()
        if not line.lstrip().startswith("--")
    )
    # T-19: render NULL results as a literal sentinel so parse_results
    # doesn't silently drop them. Test files write `<NULL>` in
    # smoke.expected for any column that should be NULL.
    sql = ".nullvalue <NULL>\n" + sql

    try:
        result = subprocess.run(
            [str(CLI_BIN), str(CLI_COMPONENT), "--db", ":memory:"],
            input=sql,
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=REPO_ROOT,
        )
    except subprocess.TimeoutExpired:
        return (False, f"timeout after {timeout}s")

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

    # Second-pass (optional): assert outputs against smoke.expected.
    expected_path = REPO_ROOT / "extensions" / name / "smoke.expected"
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
    p.add_argument("--show-parsed", metavar="NAME",
                   help="print the parsed result rows for one extension and exit "
                        "(useful when seeding smoke.expected)")
    args = p.parse_args()
    if not (args.name or args.all or args.list or args.show_parsed):
        p.error("specify <name>, --all, --list, or --show-parsed")

    if args.show_parsed:
        ok, output = smoke_one(args.show_parsed, args.timeout)
        # Re-run only to fetch fresh stdout for parsing  smoke_one
        # already did this but it's not exposed. Run again cheaply.
        smoke = REPO_ROOT / "extensions" / args.show_parsed / "smoke.sql"
        sql = "\n".join(l for l in smoke.read_text().splitlines() if not l.lstrip().startswith("--"))
        sql = ".nullvalue <NULL>\n" + sql  # T-19, see smoke_one()
        r = subprocess.run(
            [str(CLI_BIN), str(CLI_COMPONENT), "--db", ":memory:"],
            input=sql, capture_output=True, text=True, timeout=args.timeout,
        )
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
    for name in targets:
        ok, output = smoke_one(name, args.timeout)
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
