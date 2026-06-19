#!/usr/bin/env python3
"""Run cli-level smokes  the parallel of `tooling/smoke.py`,
but for cli built-in dot-commands (`.session`, `.serialize`, ...)
instead of per-extension behavior.

Smoke files live in `tooling/cli-smokes/<name>.sql` with an
optional `<name>.expected` (same expected-format as smoke.py).
The harness runs each .sql through the cli with a fresh tempdir
as cwd + `--db <tempdir>/smoke.db`, so the wasi sandbox can
write files in the same directory the smoke sees.

Usage:
    tooling/cli-smoke.py <name>      # run one
    tooling/cli-smoke.py --all       # run every smoke
    tooling/cli-smoke.py --list      # list smoke names
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CLI_BIN = REPO_ROOT / "target" / "release" / "sqlite-wasm-run"
CLI_COMPONENT = REPO_ROOT / "target" / "wasm32-wasip2" / "release" / "sqlite_cli.component.wasm"
SMOKES_DIR = REPO_ROOT / "tooling" / "cli-smokes"

PROMPT_RE = re.compile(r"^(sqlite>\s*|\s*\.\.\.>\s*)+")


def parse_results(raw: str) -> list[str]:
    """Same shape as smoke.py: strip prompts + blank lines, return
    the cli's response lines in order."""
    out: list[str] = []
    for line in raw.splitlines():
        line = PROMPT_RE.sub("", line).rstrip()
        if not line:
            continue
        out.append(line)
    return out


def parse_expected(path: Path) -> list[str]:
    """Mirror of smoke.py's expected parser. `#` + whitespace = comment.
    `~~` = skip this output. `?` = any non-empty value."""
    out: list[str] = []
    for raw in path.read_text().splitlines():
        s = raw.rstrip()
        if not s:
            continue
        if s.startswith("#") and (len(s) == 1 or s[1].isspace()):
            continue
        out.append(s)
    return out


def diff_results(actual: list[str], expected: list[str]) -> str | None:
    """Return None on match, otherwise a description of the first
    divergence. `~~` skips; `?` requires non-empty."""
    n = max(len(actual), len(expected))
    for i in range(n):
        a = actual[i] if i < len(actual) else "<missing>"
        e = expected[i] if i < len(expected) else "<unexpected>"
        if e == "~~":
            continue
        if e == "?":
            if not a or a == "<missing>":
                return f"line {i+1}: expected non-empty, got {a!r}"
            continue
        if a != e:
            return f"line {i+1}: expected {e!r}, got {a!r}"
    return None


def smoke_one(name: str, timeout: int = 30) -> tuple[bool, str]:
    if not CLI_BIN.exists():
        return False, f"cli runner not built: run cargo build --release -p sqlite-wasm-host"
    if not CLI_COMPONENT.exists():
        return False, f"cli component not built"
    sql_path = SMOKES_DIR / f"{name}.sql"
    expected_path = SMOKES_DIR / f"{name}.expected"
    if not sql_path.exists():
        return False, f"no smoke at {sql_path}"
    raw_text = sql_path.read_text()
    # Strip `--` line comments before piping  the cli's input parser
    # fuses leading `--` with the following dot-command, same wart as
    # extension smoke.py works around (T-9).
    sql = "\n".join(
        line for line in raw_text.splitlines()
        if not line.lstrip().startswith("--")
    )
    sql = ".nullvalue <NULL>\n" + sql
    tmpdir = tempfile.mkdtemp(prefix="sqlite-wasm-cli-smoke-")
    try:
        # Pass --db as a relative path so the cli's wasi preopen
        # resolves under cwd. Absolute --db preopens an unrelated
        # /var/folders/... path and breaks relative file writes
        # (e.g. .session changeset out.cs).
        argv = [str(CLI_BIN), str(CLI_COMPONENT), "--db", "smoke.db"]
        try:
            result = subprocess.run(
                argv,
                input=sql,
                capture_output=True,
                text=True,
                timeout=timeout,
                cwd=tmpdir,
            )
        except subprocess.TimeoutExpired:
            return False, f"timeout after {timeout}s"
        out = result.stdout + result.stderr
        if "panic" in out.lower() or "Error loading" in out:
            return False, f"panic/load error:\n{out}"
        if not expected_path.exists():
            return True, ""
        actual = parse_results(out)
        expected = parse_expected(expected_path)
        diff = diff_results(actual, expected)
        if diff:
            return False, f"{diff}\n--- full output ---\n{out}"
        return True, ""
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


def list_smokes() -> list[str]:
    if not SMOKES_DIR.exists():
        return []
    return sorted(p.stem for p in SMOKES_DIR.glob("*.sql"))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("name", nargs="?")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--timeout", type=int, default=30)
    args = ap.parse_args()

    if args.list:
        for n in list_smokes():
            print(n)
        return 0

    names = [args.name] if args.name else (list_smokes() if args.all else [])
    if not names:
        ap.print_usage()
        return 2

    failed = []
    for n in names:
        ok, msg = smoke_one(n, args.timeout)
        if ok:
            print(f"PASS  {n}")
        else:
            print(f"FAIL  {n}")
            for line in msg.splitlines():
                print(f"    {line}")
            failed.append(n)
    print()
    if failed:
        print(f"{len(failed)} failed: {', '.join(failed)}")
        return 1
    print(f"all {len(names)} passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
