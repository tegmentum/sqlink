#!/usr/bin/env python3
"""Run an extension's smoke.sql against the cli.

Each extension owns extensions/<name>/smoke.sql. This harness pipes
the file's statements through the cli and prints what comes back.
No assertions in v1 — failures are advisory only (the goal is "did
anything panic").

Usage:
    tooling/smoke.py <name>      # smoke one extension
    tooling/smoke.py --all       # smoke every ext that has a smoke.sql
    tooling/smoke.py --list      # list extensions with smoke.sql
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

CLI_BIN = REPO_ROOT / "target" / "release" / "sqlite-wasm-run"
CLI_COMPONENT = REPO_ROOT / "target" / "wasm32-wasip2" / "release" / "sqlite_cli.component.wasm"


def find_smoke_files() -> list[Path]:
    return sorted(REPO_ROOT.glob("extensions/*/smoke.sql"))


def smoke_one(name: str, timeout: int = 30) -> tuple[bool, str]:
    smoke = REPO_ROOT / "extensions" / name / "smoke.sql"
    if not smoke.exists():
        return (False, f"no smoke.sql at {smoke.relative_to(REPO_ROOT)}")
    if not CLI_BIN.exists():
        return (False, f"cli runner not built: {CLI_BIN.relative_to(REPO_ROOT)} missing; "
                       f"run: cargo build --release -p sqlite-wasm-host")
    if not CLI_COMPONENT.exists():
        return (False, f"cli component not built: {CLI_COMPONENT.relative_to(REPO_ROOT)} missing")

    # Strip `--` line comments before piping: the cli's parser
    # fuses a leading comment block with the following dot-command
    # and chokes on the `.` of `.load`. SQL comments later in the
    # file (inside a statement) are fine.
    sql = "\n".join(
        line for line in smoke.read_text().splitlines()
        if not line.lstrip().startswith("--")
    )

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

    # Heuristic: an extension that fails to load shows "Error loading"
    # and a missing scalar shows "no such function". A panic shows
    # "thread '<unnamed>' panicked" in stderr.
    out = result.stdout + result.stderr
    failed = any(
        marker in out
        for marker in (
            "Error loading",
            "no such function",
            "panicked",
            "instantiate loaded ext",
        )
    )
    return (not failed, out)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("name", nargs="?", help="extension to smoke")
    g.add_argument("--all", action="store_true", help="smoke every extension that has smoke.sql")
    g.add_argument("--list", action="store_true", help="list extensions with smoke.sql")
    p.add_argument("--timeout", type=int, default=30)
    args = p.parse_args()

    if args.list:
        for f in find_smoke_files():
            print(f.parent.name)
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
            for line in output.split("\n")[:20]:
                print(f"    {line}")

    if fails:
        print(f"\n{len(fails)} failed: {', '.join(fails)}")
        sys.exit(1)
    print(f"\nall {len(targets)} passed")


if __name__ == "__main__":
    main()
