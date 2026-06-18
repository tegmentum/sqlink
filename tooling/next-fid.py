#!/usr/bin/env python3
"""Print the next unused FID const value for an extension.

When adding a new scalar to an existing extension, I keep grepping
for `const FID_*: u64 = N` and counting forward. This prints the
max + 1 directly.

Usage:
  python3 tooling/next-fid.py <name>
  python3 tooling/next-fid.py xor
    3

Exits 0 with the number on stdout. Exits 1 if no FID consts found
(probably means the lib.rs is still scaffolded with the placeholder).
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

FID = re.compile(r"const\s+FID_[A-Z0-9_]+\s*:\s*u64\s*=\s*(\d+)\s*;")


def main() -> None:
    if len(sys.argv) != 2:
        print("usage: next-fid.py <name>", file=sys.stderr)
        sys.exit(2)
    name = sys.argv[1]
    lib = REPO_ROOT / "extensions" / name / "src" / "lib.rs"
    if not lib.exists():
        print(f"no extensions/{name}/src/lib.rs", file=sys.stderr)
        sys.exit(1)
    fids = [int(m.group(1)) for m in FID.finditer(lib.read_text())]
    if not fids:
        print(f"no FID consts found in {lib.relative_to(REPO_ROOT)} "
              f"(scaffold not edited yet?)", file=sys.stderr)
        sys.exit(1)
    print(max(fids) + 1)


if __name__ == "__main__":
    main()
