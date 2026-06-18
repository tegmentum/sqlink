#!/usr/bin/env python3
"""Check that extension names referenced in tooling docs still exist.

The catalog has ~108 extensions and growing. Three docs cite specific
extensions as reference / consumer / surfaced-by:

  tooling/extension-patterns.md   shape  representative extension
  tooling/snippets/README.md      snippet  consumer extensions
  tooling/cli-cheatsheet.md       harness limitation  surfacing ext

If an extension gets renamed or removed, these citations rot silently.
This tool walks the three docs, extracts cited names, and flags any
that don't exist under `extensions/<name>/`.

Patterns matched (deliberately narrow to avoid false positives):
  - `Reference: ` <name> followed by punctuation
  - `Consumers?: ` <name> [, <name> ...]
  - `Surfaced via ` <name>
  - Quick-picker table rows (extension-patterns.md): the last column
    is a comma-separated list of extension names.

Usage:
  python3 tooling/doc-refs-check.py
  exit 0 = all references valid
  exit 1 = orphan(s) found
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
EXT_DIR = REPO_ROOT / "extensions"
DOCS = [
    REPO_ROOT / "tooling" / "extension-patterns.md",
    REPO_ROOT / "tooling" / "snippets" / "README.md",
    REPO_ROOT / "tooling" / "cli-cheatsheet.md",
]

# `name`  the captured name. Length 3+; alnum + hyphen + underscore.
NAME = r"`([a-z][a-z0-9_-]{2,})`"
PATTERNS = [
    (re.compile(rf"Reference:\s*{NAME}"),    "Reference"),
    (re.compile(rf"Consumers?:\s*([^\n]+)"), "Consumers"),
    (re.compile(rf"Surfaced via\s*{NAME}"),  "Surfaced via"),
]

# Last column of the extension-patterns.md picker table. Match any
# 3-column row whose first column is a capitalized shape name and
# whose third column looks like a comma-separated list of names.
# Skips the table header (which starts with "|----" or "| Shape").
PICKER_ROW = re.compile(
    r"^\|\s*([A-Z][^|]*?)\s*\|([^|]+)\|([^|]+)\|", re.MULTILINE)


def all_extensions() -> set[str]:
    """Set of extension directory names that exist on disk."""
    return {p.name for p in EXT_DIR.iterdir() if p.is_dir()}


def extract_refs(text: str) -> set[str]:
    """Pull all extension-name references from a doc body."""
    refs: set[str] = set()
    # Marker-prefixed references.
    for pat, _ in PATTERNS:
        for m in pat.finditer(text):
            payload = m.group(1)
            # "Consumers" payload is a comma-list with backticked names;
            # other patterns capture the bare name.
            for n in re.findall(NAME, payload):
                refs.add(n)
            # Patterns 0 and 2 capture the name directly  the
            # findall above covers them too.
    # Picker-table last-column entries: comma-separated bare names.
    # Match any 3-column row; the second group is the last column.
    # Skip header rows whose first column is "Shape" or contains
    # only dashes / pipes.
    for m in PICKER_ROW.finditer(text):
        shape = m.group(1).strip()
        if shape.lower() == "shape" or set(shape) <= {"-", " "}:
            continue
        for n in m.group(3).split(","):
            n = n.strip()
            if re.fullmatch(r"[a-z][a-z0-9_-]{2,}", n):
                refs.add(n)
    return refs


def main() -> None:
    exts = all_extensions()
    orphans: list[tuple[str, str]] = []
    total_refs = 0
    for doc in DOCS:
        if not doc.exists():
            print(f"missing doc: {doc.relative_to(REPO_ROOT)}", file=sys.stderr)
            continue
        refs = extract_refs(doc.read_text())
        total_refs += len(refs)
        for r in sorted(refs):
            if r not in exts:
                orphans.append((doc.relative_to(REPO_ROOT).as_posix(), r))
    print(f"checked {total_refs} reference(s) across {len(DOCS)} doc(s)")
    if orphans:
        print(f"\n{len(orphans)} orphan reference(s):")
        for doc, name in orphans:
            print(f"  {doc}: `{name}` (no extensions/{name}/)")
        sys.exit(1)
    print("all references map to existing extensions")


if __name__ == "__main__":
    main()
