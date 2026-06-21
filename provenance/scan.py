#!/usr/bin/env python3
"""Scan extensions/* and populate provenance/extensions.db.

Usage:
    python3 provenance/scan.py [--db PATH] [--extensions-dir PATH]

For each extensions/<name>/ subdirectory:
  * Parses Cargo.toml (if present) for name/version/license/authors/
    description/edition/dependencies.
  * Recursively hashes the src/ tree (sha-256 over sorted-path-prefixed
    file contents) so the source state is content-addressed and
    re-running on unchanged source produces an unchanged version row.
  * Greps the Rust source for ScalarFunctionSpec / AggregateFunctionSpec
    / VtabSpec entries to populate sql_function.
  * Greps for `declared_capabilities: alloc::vec![Capability::Http, ...]`
    to populate capability.
  * Looks for built .wasm / .component.wasm artifacts under
    extensions/<name>/target/wasm32-wasip2/release/ AND under the
    top-level target/wasm32-wasip2/release/ (the stats extension
    builds into the workspace target, not its own).
  * Detects the world the extension uses (minimal / tabular /
    stateful / minimal-http / collating / authorizing / hooked /
    resolving) by grepping for `world: "..."` in the wit_bindgen
    macro call.

C-bundled plugins (no Cargo.toml; just a .c file) get a minimal
record: kind='c-bundled', source_sha256 over the .c file, no
deps, hand-curated description from a sidecar file or the C
comments.

Re-running is idempotent: a (plugin, version, source_sha256)
that already exists is upserted, not duplicated.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import sqlite3
import subprocess
import sys
import time
from typing import Iterable, Optional

try:
    import tomllib  # Python 3.11+
except ImportError:
    import tomli as tomllib  # type: ignore

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_DB = REPO_ROOT / "provenance" / "extensions.db"
DEFAULT_EXT = REPO_ROOT / "extensions"
DEFAULT_TARGET = REPO_ROOT / "target" / "wasm32-wasip2" / "release"


# ─── source hashing ────────────────────────────────────────

def hash_tree(root: pathlib.Path) -> tuple[str, int, int]:
    """SHA-256 of (relpath || NUL || contents || NUL) over every file
    under `root`, sorted by relpath. Returns (hex_digest, file_count,
    total_bytes).

    target/ is excluded (build artifacts aren't source). Cargo.lock
    is included  it's source-state too for reproducibility.
    """
    h = hashlib.sha256()
    files = 0
    bytes_total = 0
    paths: list[pathlib.Path] = []
    for p in root.rglob("*"):
        if not p.is_file():
            continue
        # Exclude build output.
        rel = p.relative_to(root)
        parts = set(rel.parts)
        if "target" in parts:
            continue
        paths.append(p)
    paths.sort()
    for p in paths:
        rel = str(p.relative_to(root)).replace("\\", "/")
        h.update(rel.encode("utf-8"))
        h.update(b"\x00")
        data = p.read_bytes()
        h.update(data)
        h.update(b"\x00")
        files += 1
        bytes_total += len(data)
    return h.hexdigest(), files, bytes_total


def sha256_file(p: pathlib.Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


# ─── git ───────────────────────────────────────────────────

def git_head_sha() -> Optional[str]:
    try:
        out = subprocess.check_output(
            ["git", "-C", str(REPO_ROOT), "rev-parse", "HEAD"],
            stderr=subprocess.DEVNULL,
        )
        return out.decode().strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


# ─── Rust extension ─────────────────────────────────────────

WORLD_RE = re.compile(r'world:\s*"([\w-]+)"')
SCALAR_RE = re.compile(
    r'ScalarFunctionSpec\s*\{[^}]*?id:\s*([A-Z_0-9]+)[^}]*?name:\s*"([^"]+)"[^}]*?num_args:\s*(-?\d+)',
    re.DOTALL,
)
# Also catch the `s(FID_X, "name", N, ...)` helper form used in
# most extensions.
SCALAR_HELPER_RE = re.compile(r'\bs\(\s*[A-Za-z_0-9]+,\s*"([^"]+)",\s*(-?\d+)')
AGG_RE = re.compile(
    r'AggregateFunctionSpec\s*\{[^}]*?name:\s*"([^"]+)"[^}]*?num_args:\s*(-?\d+)',
    re.DOTALL,
)
AGG_HELPER_RE = re.compile(r'\ba\(\s*[A-Za-z_0-9]+,\s*"([^"]+)",\s*(-?\d+)')
VTAB_RE = re.compile(
    r'VtabSpec\s*\{[^}]*?name:\s*"([^"]+)"[^}]*?eponymous:\s*(true|false)',
    re.DOTALL,
)
CAP_RE = re.compile(r'Capability::(\w+)')
DECLARED_CAPS_RE = re.compile(
    r'declared_capabilities:\s*alloc::vec!\[(.*?)\]',
    re.DOTALL,
)


def detect_world(src_text: str) -> Optional[str]:
    m = WORLD_RE.search(src_text)
    return m.group(1) if m else None


def extract_functions(src_text: str) -> list[tuple[str, str, Optional[int], Optional[str]]]:
    """Return list of (kind, name, num_args, flags). flags is currently
    None because the helper form doesn't expose it inline; future work
    can parse the surrounding `FunctionFlags::` literal if needed.
    """
    out: list[tuple[str, str, Optional[int], Optional[str]]] = []
    # Scalar
    for name, n in SCALAR_HELPER_RE.findall(src_text):
        out.append(("scalar", name, int(n), None))
    for _fid, name, n in SCALAR_RE.findall(src_text):
        # Avoid duplicates with the helper form (same name AND num_args).
        key = ("scalar", name, int(n))
        if not any(o[0] == key[0] and o[1] == key[1] and o[2] == key[2] for o in out):
            out.append((*key, None))
    # Aggregate
    for name, n in AGG_HELPER_RE.findall(src_text):
        out.append(("aggregate", name, int(n), None))
    for name, n in AGG_RE.findall(src_text):
        key = ("aggregate", name, int(n))
        if not any(o[0] == key[0] and o[1] == key[1] and o[2] == key[2] for o in out):
            out.append((*key, None))
    # Vtab
    for name, ep in VTAB_RE.findall(src_text):
        out.append(("vtab", name, None, "eponymous" if ep == "true" else None))
    return out


def extract_capabilities(src_text: str) -> list[str]:
    m = DECLARED_CAPS_RE.search(src_text)
    if not m:
        return []
    body = m.group(1)
    return CAP_RE.findall(body)


def read_cargo(path: pathlib.Path) -> dict:
    with path.open("rb") as f:
        return tomllib.load(f)


def parse_deps(cargo: dict) -> list[tuple[str, str, str, bool, str]]:
    """[(name, version_req, source, optional, features_csv), ...]"""
    out: list[tuple[str, str, str, bool, str]] = []
    deps = cargo.get("dependencies", {})
    for name, spec in deps.items():
        if isinstance(spec, str):
            out.append((name, spec, "crates.io", False, ""))
            continue
        version_req = spec.get("version", "") if isinstance(spec, dict) else ""
        optional = bool(spec.get("optional", False)) if isinstance(spec, dict) else False
        features = ",".join(spec.get("features", [])) if isinstance(spec, dict) else ""
        if isinstance(spec, dict):
            if "path" in spec:
                source = f"path:{spec['path']}"
            elif "git" in spec:
                source = f"git+{spec['git']}"
                if "rev" in spec:
                    source += f"#{spec['rev']}"
            else:
                source = "crates.io"
        else:
            source = "crates.io"
        out.append((name, version_req, source, optional, features))
    return out


# ─── C-bundled extension ───────────────────────────────────

C_BUNDLED_FALLBACK = {
    "fts5": ("fts5 vtab (full-text search) bundled via libsqlite3-sys", "MIT-Public-Domain"),
    "rtree": ("rtree vtab (spatial / range index) bundled via libsqlite3-sys", "MIT-Public-Domain"),
    "geopoly": ("geopoly vtab (polygon spatial) via LIBSQLITE3_FLAGS=-DSQLITE_ENABLE_GEOPOLY", "MIT-Public-Domain"),
    "wasm-demo": ("Demo C extension for the wasm-component dispatch model", "MIT"),
}


def scan_c_bundled(plugin_dir: pathlib.Path) -> Optional[dict]:
    c_files = list(plugin_dir.glob("*.c"))
    if not c_files:
        return None
    desc, lic = C_BUNDLED_FALLBACK.get(plugin_dir.name, (None, None))
    digest, files, bytes_total = hash_tree(plugin_dir)
    return {
        "name": plugin_dir.name,
        "kind": "c-bundled",
        "path": str(plugin_dir.relative_to(REPO_ROOT)),
        "description": desc,
        "declared_world": None,
        "version": "bundled",
        "license": lic,
        "authors": "SQLite contributors (public-domain) + maintainers",
        "edition": None,
        "source_sha256": digest,
        "src_file_count": files,
        "src_byte_count": bytes_total,
        "deps": [],
        "functions": [],
        "capabilities": [],
        "world": None,
        "src_text": "",
    }


# ─── Rust extension entry ──────────────────────────────────

def scan_rust(plugin_dir: pathlib.Path) -> Optional[dict]:
    cargo_path = plugin_dir / "Cargo.toml"
    if not cargo_path.exists():
        return None
    try:
        cargo = read_cargo(cargo_path)
    except Exception as e:
        print(f"  warning: failed to parse {cargo_path}: {e}", file=sys.stderr)
        return None
    pkg = cargo.get("package", {})
    name = pkg.get("name", plugin_dir.name).removesuffix("-extension")
    version = pkg.get("version", "0.0.0")
    description = pkg.get("description", "").strip() or None
    license = pkg.get("license")
    authors = pkg.get("authors") or []
    if isinstance(authors, list):
        authors = ";".join(authors) if authors else None
    edition = pkg.get("edition")
    # Source content hash + introspection.
    digest, files, bytes_total = hash_tree(plugin_dir)
    src_text = ""
    for rs in plugin_dir.rglob("*.rs"):
        if "target" in rs.parts:
            continue
        try:
            src_text += rs.read_text(errors="replace") + "\n"
        except Exception:
            pass
    world = detect_world(src_text)
    functions = extract_functions(src_text)
    caps = extract_capabilities(src_text)
    deps = parse_deps(cargo)
    return {
        "name": plugin_dir.name,
        "kind": "rust",
        "path": str(plugin_dir.relative_to(REPO_ROOT)),
        "description": description,
        "declared_world": world,
        "version": version,
        "license": license,
        "authors": authors,
        "edition": edition,
        "source_sha256": digest,
        "src_file_count": files,
        "src_byte_count": bytes_total,
        "deps": deps,
        "functions": functions,
        "capabilities": caps,
        "world": world,
        "src_text": src_text,
    }


# ─── artifact discovery ───────────────────────────────────

def find_artifacts(plugin_dir: pathlib.Path, plugin_name: str) -> list[dict]:
    """Look in both plugin-local and workspace-root target dirs."""
    candidates: list[pathlib.Path] = []
    locations = [
        plugin_dir / "target" / "wasm32-wasip2" / "release",
        DEFAULT_TARGET,
    ]
    # Cargo replaces hyphens with underscores in artifact filenames.
    stem_options = [
        plugin_name,
        plugin_name.replace("-", "_"),
        f"{plugin_name.replace('-', '_')}_extension",
        f"{plugin_name}-extension",
    ]
    for loc in locations:
        if not loc.exists():
            continue
        for stem in stem_options:
            for suffix in (".wasm", ".component.wasm"):
                p = loc / f"{stem}{suffix}"
                if p.exists():
                    candidates.append(p)
    # Dedup.
    seen = set()
    out: list[dict] = []
    for p in candidates:
        if p in seen:
            continue
        seen.add(p)
        try:
            sha = sha256_file(p)
            size = p.stat().st_size
            mtime = int(p.stat().st_mtime)
        except OSError:
            continue
        kind = "component-wasm" if p.name.endswith(".component.wasm") else "core-wasm"
        out.append({
            "kind": kind,
            "path": str(p.relative_to(REPO_ROOT)),
            "sha256": sha,
            "size_bytes": size,
            "target_triple": "wasm32-wasip2",
            "adapter": None,
            "built_at": mtime,
        })
    return out


# ─── DB ingest ────────────────────────────────────────────

def init_db(db_path: pathlib.Path) -> sqlite3.Connection:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(db_path))
    schema = (REPO_ROOT / "provenance" / "schema.sql").read_text()
    conn.executescript(schema)
    # Re-seed the candidate survey on every scan so the SQL file
    # in candidates.sql is the source of truth. Drop-and-replace
    # idempotent  candidate edits land in candidates.sql, scan.py
    # picks them up. Safe to run on every `make ext`.
    candidates_path = REPO_ROOT / "provenance" / "candidates.sql"
    if candidates_path.exists():
        try:
            conn.executescript(candidates_path.read_text())
        except sqlite3.Error as e:
            # Don't let a malformed candidates.sql tank the
            # build  surface a warning and move on.
            print(f"warning: candidates.sql failed to apply: {e}", file=sys.stderr)
    return conn


def load_upstream_urls() -> dict:
    """Load the curated name -> upstream URL map for shipped extensions.

    The file lives at provenance/upstream-urls.json. Keys starting with
    underscore are treated as section dividers / comments and ignored.
    Missing entries leave plugin.upstream_url as NULL  the per-extension
    page hides the "upstream" row when empty."""
    path = REPO_ROOT / "provenance" / "upstream-urls.json"
    if not path.exists():
        return {}
    try:
        data = json.loads(path.read_text())
    except Exception as e:
        print(f"warning: upstream-urls.json failed to parse: {e}", file=sys.stderr)
        return {}
    return {k: v for k, v in data.items() if not k.startswith("_") and isinstance(v, str)}


_UPSTREAM_URLS = load_upstream_urls()


def upsert_plugin(conn: sqlite3.Connection, info: dict, commit_sha: Optional[str]) -> None:
    cur = conn.cursor()
    upstream_url = _UPSTREAM_URLS.get(info["name"])
    cur.execute(
        "INSERT INTO plugin(name, kind, path, description, declared_world, upstream_url, notes) "
        "VALUES (?, ?, ?, ?, ?, ?, NULL) "
        "ON CONFLICT(name) DO UPDATE SET "
        "  kind=excluded.kind, "
        "  path=excluded.path, "
        "  description=excluded.description, "
        "  declared_world=excluded.declared_world, "
        "  upstream_url=excluded.upstream_url",
        (info["name"], info["kind"], info["path"], info["description"],
         info["declared_world"], upstream_url),
    )
    cur.execute("SELECT id FROM plugin WHERE name = ?", (info["name"],))
    plugin_id = cur.fetchone()[0]

    # Insert version (idempotent on (plugin_id, version, source_sha256)).
    now = int(time.time())
    cur.execute(
        "INSERT OR IGNORE INTO plugin_version("
        "  plugin_id, version, license, authors, edition, "
        "  source_sha256, src_file_count, src_byte_count, "
        "  commit_sha, scanned_at"
        ") VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        (plugin_id, info["version"], info["license"], info["authors"],
         info["edition"], info["source_sha256"], info["src_file_count"],
         info["src_byte_count"], commit_sha, now),
    )
    cur.execute(
        "SELECT id FROM plugin_version WHERE plugin_id = ? AND version = ? AND source_sha256 = ?",
        (plugin_id, info["version"], info["source_sha256"]),
    )
    pv_id = cur.fetchone()[0]

    # Refresh dependency rows for this version.
    cur.execute("DELETE FROM dependency WHERE plugin_version_id = ?", (pv_id,))
    for name, vreq, src, opt, feats in info["deps"]:
        cur.execute(
            "INSERT INTO dependency(plugin_version_id, name, version_req, source, optional, features) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            (pv_id, name, vreq or None, src, int(opt), feats or None),
        )

    # Refresh sql_function rows.
    cur.execute("DELETE FROM sql_function WHERE plugin_version_id = ?", (pv_id,))
    for kind, fname, n, flags in info["functions"]:
        cur.execute(
            "INSERT INTO sql_function(plugin_version_id, kind, name, num_args, flags) "
            "VALUES (?, ?, ?, ?, ?)",
            (pv_id, kind, fname, n, flags),
        )

    # Refresh capabilities.
    cur.execute("DELETE FROM capability WHERE plugin_version_id = ?", (pv_id,))
    for cap in info["capabilities"]:
        cur.execute(
            "INSERT OR IGNORE INTO capability(plugin_version_id, name) VALUES (?, ?)",
            (pv_id, cap),
        )

    # Refresh artifact rows.
    cur.execute("DELETE FROM artifact WHERE plugin_version_id = ?", (pv_id,))
    plugin_dir = REPO_ROOT / info["path"]
    for art in find_artifacts(plugin_dir, info["name"]):
        cur.execute(
            "INSERT INTO artifact(plugin_version_id, kind, path, sha256, size_bytes, "
            "                     target_triple, adapter, built_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (pv_id, art["kind"], art["path"], art["sha256"], art["size_bytes"],
             art["target_triple"], art["adapter"], art["built_at"]),
        )

    conn.commit()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=str(DEFAULT_DB), help="DB path (default: provenance/extensions.db)")
    ap.add_argument("--extensions-dir", default=str(DEFAULT_EXT))
    args = ap.parse_args()

    db_path = pathlib.Path(args.db)
    ext_dir = pathlib.Path(args.extensions_dir)
    if not ext_dir.is_dir():
        print(f"error: extensions dir not found: {ext_dir}", file=sys.stderr)
        return 2

    conn = init_db(db_path)
    commit_sha = git_head_sha()
    print(f"scanning {ext_dir}  {db_path} (HEAD={commit_sha or 'unknown'})")

    scanned = 0
    skipped = []
    for sub in sorted(ext_dir.iterdir()):
        if not sub.is_dir():
            continue
        # Skip tooling-managed dirs that look like extensions but aren't.
        # Leading `_` is the convention.
        if sub.name.startswith("_"):
            continue
        info = scan_rust(sub) or scan_c_bundled(sub)
        if not info:
            skipped.append(sub.name)
            continue
        upsert_plugin(conn, info, commit_sha)
        print(f"  + {info['name']:<22} {info['kind']:<10} v{info['version']:<12} {info['source_sha256'][:12]}  funcs={len(info['functions'])} deps={len(info['deps'])}")
        scanned += 1

    if skipped:
        print(f"skipped (no Cargo.toml or .c file): {', '.join(skipped)}")
    print(f"done  {scanned} plugin versions scanned into {db_path}")
    conn.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
