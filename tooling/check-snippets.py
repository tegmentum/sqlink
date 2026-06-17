#!/usr/bin/env python3
"""Verify inlined snippets in extensions/*/src/lib.rs match source.

Convention (established in tooling/snippets/README.md):

    // --- snippet: tooling/snippets/luhn.rs (weighted_mod10) ---
    fn weighted_mod10(digits: &str, weights: &[u32]) -> Option<bool> {
        ...
    }
    // --- end snippet ---

The opening line names the source file and (optionally, in parens)
which top-level fn within that source. The block between the
delimiters should match the named fn's body in the source.

What we compare:
  * The function signature line (the `fn name(...) -> ... {`)
  * The body lines (everything until the matching closing brace)
  * Normalized: leading whitespace per line stripped, blank lines
    coalesced. Doc comments and attributes in the source are NOT
    expected in the inlined copy (consumers strip them).

Usage:
    tooling/check-snippets.py             # check all extensions
    tooling/check-snippets.py <name>      # check one extension

Exit codes:
    0  all snippets match source
    1  one or more drift; details printed
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

SNIPPET_OPEN = re.compile(
    r"//\s*---\s*snippet:\s*(?P<path>[^\s()]+)\s*(?:\((?P<fn>[^)]+)\))?\s*---"
)
SNIPPET_CLOSE = re.compile(r"//\s*---\s*end\s+snippet\s*---")


def extract_fn_block(source: str, fn_name: str) -> list[str] | None:
    """Return the normalized fn definition (signature + body) from source.

    Match starts at `fn <name>(`; brace-counts to find the matching close.
    Strips per-line leading whitespace + trailing whitespace; skips
    blank lines.
    """
    sig_re = re.compile(rf"^\s*fn\s+{re.escape(fn_name)}\s*[(<]", re.MULTILINE)
    m = sig_re.search(source)
    if not m:
        return None
    start = m.start()
    # Find first `{` at-or-after the signature start; from there
    # brace-count to the matching close.
    open_idx = source.find("{", start)
    if open_idx < 0:
        return None
    depth = 1
    i = open_idx + 1
    while i < len(source) and depth > 0:
        c = source[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
        i += 1
    # i now points just past the matching close brace
    raw = source[start:i]
    return normalize_block(raw)


def normalize_block(text: str) -> list[str]:
    out: list[str] = []
    for line in text.splitlines():
        s = line.strip()
        if not s:
            continue
        # Skip standalone doc-comment / attribute lines  consumers
        # drop these. Inline `///` after code wouldn't be valid Rust
        # anyway, so a simple startswith check suffices.
        if s.startswith("///") or s.startswith("//!"):
            continue
        if s.startswith("#["):
            continue
        out.append(s)
    return out


def find_inlined_blocks(lib_rs: Path) -> list[tuple[int, str, str | None, list[str]]]:
    """Return list of (line_no, source_path, fn_name_or_None, inlined_block)."""
    text = lib_rs.read_text()
    lines = text.splitlines()
    blocks: list[tuple[int, str, str | None, list[str]]] = []
    i = 0
    while i < len(lines):
        m = SNIPPET_OPEN.search(lines[i])
        if not m:
            i += 1
            continue
        start_line = i + 1  # 1-indexed
        source_path = m.group("path")
        fn_name = m.group("fn")
        # Find the matching close
        j = i + 1
        body_lines: list[str] = []
        while j < len(lines) and not SNIPPET_CLOSE.search(lines[j]):
            body_lines.append(lines[j])
            j += 1
        if j >= len(lines):
            blocks.append(
                (start_line, source_path, fn_name, [f"<unterminated snippet block>"])
            )
            break
        normalized = normalize_block("\n".join(body_lines))
        blocks.append((start_line, source_path, fn_name, normalized))
        i = j + 1
    return blocks


def check_extension(name: str) -> list[str]:
    """Return list of human-readable drift descriptions (empty = clean)."""
    lib_rs = REPO_ROOT / "extensions" / name / "src" / "lib.rs"
    if not lib_rs.exists():
        return [f"{name}: no src/lib.rs"]
    issues: list[str] = []
    for line_no, src_path, fn_name, inlined in find_inlined_blocks(lib_rs):
        source_file = REPO_ROOT / src_path
        if not source_file.exists():
            issues.append(f"{name} (lib.rs:{line_no}): source {src_path} not found")
            continue
        source_text = source_file.read_text()
        if fn_name is None:
            # Whole-file mode: normalize source minus header comments
            expected = normalize_block(source_text)
        else:
            expected = extract_fn_block(source_text, fn_name)
            if expected is None:
                issues.append(
                    f"{name} (lib.rs:{line_no}): fn `{fn_name}` not found in {src_path}"
                )
                continue
        if expected != inlined:
            diff = describe_diff(expected, inlined)
            issues.append(
                f"{name} (lib.rs:{line_no}): drift from {src_path}"
                + (f" ({fn_name})" if fn_name else "")
                + "\n" + diff
            )
    return issues


def describe_diff(expected: list[str], actual: list[str]) -> str:
    """Single-pass unified-ish diff: line index + which side has what."""
    out: list[str] = []
    n = max(len(expected), len(actual))
    for i in range(n):
        e = expected[i] if i < len(expected) else "<missing>"
        a = actual[i] if i < len(actual) else "<missing>"
        if e != a:
            out.append(f"        line {i+1}:")
            out.append(f"          expected: {e!r}")
            out.append(f"          actual:   {a!r}")
            if len(out) >= 12:
                out.append("        ... (more drift hidden)")
                break
    return "\n".join(out) if out else "        (no per-line drift, length differs)"


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("name", nargs="?", help="extension to check (default: all)")
    args = p.parse_args()

    if args.name:
        targets = [args.name]
    else:
        targets = sorted(d.name for d in (REPO_ROOT / "extensions").iterdir()
                         if d.is_dir() and not d.name.startswith("_")
                         and (d / "src" / "lib.rs").exists())

    total_issues: list[str] = []
    for name in targets:
        issues = check_extension(name)
        total_issues.extend(issues)
        if issues:
            for issue in issues:
                print(f"DRIFT  {issue}")
        # For clean exts with snippet usage, give a positive line.
        blocks = find_inlined_blocks(REPO_ROOT / "extensions" / name / "src" / "lib.rs")
        if blocks and not issues:
            print(f"OK     {name}  {len(blocks)} snippet(s) match source")

    if total_issues:
        print(f"\n{len(total_issues)} drift issue(s) across {len(targets)} extension(s)")
        sys.exit(1)
    print("\nall snippets match source")


if __name__ == "__main__":
    main()
