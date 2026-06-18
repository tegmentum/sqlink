#!/usr/bin/env python3
"""Emit a lessons-learned.md entry stub for paste-in after a ship.

Every lessons-learned entry follows the same shape:

  ### YYYY-MM-DD  <name> [extension|investigation]

  **What I built:** ...
  **What worked:** ...
  **What surprised me:** ...
  **Tooling opportunity:** ...

This tool prints that skeleton to stdout with today's date filled
in, so I can paste-and-fill rather than re-typing the section
headers and copy-pasting yesterday's date.

Usage:
  python3 tooling/lessons-stub.py mycoolext              # plugin entry
  python3 tooling/lessons-stub.py --kind investigation T-37 "scope"
"""
from __future__ import annotations

import argparse
import datetime
import sys


PLUGIN_TEMPLATE = """---

### {date}  {name} extension

**What I built:**

**What worked:**
-

**What surprised me:**
-

**Tooling opportunity:**
- (none new) Plugin count <prev>  <next>.
"""

INVESTIGATION_TEMPLATE = """---

### {date}  {name} investigation ({scope})

**What I built:**

**What worked:**
-

**What surprised me:**
-

**Tooling opportunity:**
- ({name} closed)
"""


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("name", help="extension name OR T-* identifier")
    p.add_argument("scope", nargs="?",
                   help="(investigation kind only) short title")
    p.add_argument("--kind", choices=["plugin", "investigation"],
                   default="plugin")
    args = p.parse_args()

    today = datetime.date.today().isoformat()

    if args.kind == "investigation":
        if not args.scope:
            print("--kind investigation requires a scope arg", file=sys.stderr)
            sys.exit(1)
        print(INVESTIGATION_TEMPLATE.format(
            date=today, name=args.name, scope=args.scope))
    else:
        print(PLUGIN_TEMPLATE.format(date=today, name=args.name))


if __name__ == "__main__":
    main()
