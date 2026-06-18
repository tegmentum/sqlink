#!/usr/bin/env python3
"""Append a row to PLAN-sqlite-plugins.md's surface table.

Usage:
    tooling/plan-add.py <name> <scalars-count> "<description>"

Example:
    tooling/plan-add.py detect 5 "slug/lang/mime detection"

Produces:
    | detect (slug/lang/mime)         |    +5  | extensions/detect                  |

Column widths match the existing table in the plan doc.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PLAN = REPO_ROOT / "PLAN-sqlite-plugins.md"

# Column widths copied from existing rows
NAME_W = 31
SCALARS_W = 5
DETAIL_W = 34


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("name", help="extension name (becomes the table key)")
    p.add_argument("scalars", type=int, help="number of scalars added")
    p.add_argument("description", help="short description for the table")
    p.add_argument("--force", action="store_true",
                   help="truncate description rather than refuse (T-31)")
    args = p.parse_args()

    if not PLAN.exists():
        sys.exit(f"missing {PLAN.relative_to(REPO_ROOT)}")

    label = f"{args.name} ({args.description})"
    if len(label) > NAME_W:
        if args.force:
            label = label[:NAME_W - 1] + ""
        else:
            # T-31: silent truncation made the table misleading
            # (5+ recent ships had their descriptions silently cut).
            # Refuse by default; --force restores the old behavior.
            budget = NAME_W - len(args.name) - 3  # name + " ()" overhead
            sys.exit(
                f"description too long: '{args.description}' is "
                f"{len(args.description)} chars but column budget is "
                f"{budget} (label width {NAME_W}, name takes "
                f"{len(args.name) + 3} chars including parens).\n"
                f"hint: pick a shorter description, or pass --force "
                f"to truncate anyway."
            )
    scalar_str = f"+{args.scalars}"
    detail = f"extensions/{args.name}"
    row = f"| {label:<{NAME_W}} | {scalar_str:>{SCALARS_W}}  | {detail:<{DETAIL_W}} |"

    text = PLAN.read_text()
    # Insert before the "fts5 vtab" line which is consistently the last
    # extension-table row in the recent edits to the plan doc.
    marker = re.compile(r"^\| fts5 vtab\b", flags=re.MULTILINE)
    m = marker.search(text)
    if not m:
        # Fallback: append at end of file.
        new_text = text.rstrip("\n") + "\n" + row + "\n"
    else:
        new_text = text[: m.start()] + row + "\n" + text[m.start():]

    PLAN.write_text(new_text)
    print("appended:")
    print(row)


if __name__ == "__main__":
    main()
