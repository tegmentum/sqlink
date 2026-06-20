#!/usr/bin/env python3
"""Generate registry/index.json from provenance/extensions.db.

The provenance DB is the source of truth for what lives in extensions/.
The registry/index.json is the install-time manifest consumed by
tools/sqlite-wasm-ext and tests/cli/test-cli.sh. Until now the registry
had 9 hand-curated stub entries from before any extension shipped; this
script regenerates the file from the scanned source data so it stays
in sync as the catalog grows.

Run:
    python3 provenance/build_registry.py

Outputs to registry/index.json. Idempotent: same DB state in => same
file out (modulo the `updated` timestamp).

Hooked into `make ext` immediately after `provenance/scan.py` so a
single build keeps both files current.
"""

from __future__ import annotations

import argparse
import datetime
import json
import sqlite3
import subprocess
import sys
from pathlib import Path


REGISTRY_VERSION = "1.0.0"
REGISTRY_URL = (
    "https://raw.githubusercontent.com/user/sqlite-wasm-extensions/main/registry/index.json"
)
MIN_SQLITE_VERSION = "3.39.0"

# Default artifact base. The artifact is served from the in-database
# CAS table baked into extensions-site/registry.db  the route
# /asset/<name> returns the raw .component.wasm bytes via the
# httpd's `blob` route kind. The empty base means artifact_url is
# emitted as a relative path; consumers that need an absolute URL
# (the cli's `tools/sqlite-wasm-ext` installer, programmatic clients)
# override with --artifact-base https://extensions.example.com.
DEFAULT_ARTIFACT_BASE = ""


def commit_sha(repo: Path) -> str | None:
    try:
        r = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=repo,
            check=True,
            capture_output=True,
            text=True,
        )
        return r.stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def categorize(name: str, world: str | None) -> list[str]:
    """Bucket the extension into one of the registry's category labels.

    Keeps the entry's `categories` array meaningful for search even
    though we don't have first-class category metadata in Cargo.toml.
    The choice is name-driven (substring matching) so adding a new
    extension automatically picks a reasonable bucket  no manual
    curation needed for the common cases.
    """
    n = name.lower()
    # World-driven first; vtab is structural regardless of domain.
    if world in {"tabular", "stateful"}:
        # Could be scalars in a tabular world too; fall through to name match.
        pass

    buckets = [
        (
            "crypto",
            (
                "jwt",
                "pwhash",
                "aead",
                "totp",
                "hkdf",
                "secp256k1",
                "ssh-key",
                "tls-cert",
                "asn1",
                "crypto",
                "blake3",
                "sha3",
                "hashes-fast",
                "crc",
            ),
        ),
        (
            "id",
            ("uuid", "ulid", "nanoid", "ids"),
        ),
        (
            "compression",
            ("lz4", "zstd", "compress"),
        ),
        (
            "codec",
            (
                "binary-codecs",
                "toml",
                "bson",
                "ical",
                "vcard",
                "ean",
                "formats",
                "codecs",
            ),
        ),
        (
            "network",
            (
                "dns",
                "ipaddr",
                "idna",
                "mailto",
                "useragent",
                "publicsuffix",
                "mac",
                "mac-oui",
                "url",
                "phone",
                "http",
                "graphql",
                "email",
            ),
        ),
        (
            "bibliographic",
            ("aba", "bic", "cusip", "creditcard", "isin", "ssn", "vin", "bibcodes"),
        ),
        (
            "text-processing",
            (
                "faker",
                "regexp",
                "sentiment",
                "hexdump",
                "basen",
                "emoji",
                "morse",
                "escape",
                "text",
                "fuzzy",
                "stemmer",
                "unicode",
                "lang-detect",
                "bpe",
                "lorem",
                "case",
                "nato",
                "roman",
                "spellfix1",
            ),
        ),
        (
            "geo",
            (
                "h3",
                "s2",
                "geo",
                "postcode",
                "zorder",
                "mgrs",
                "latlon",
                "rtree",
                "geopoly",
                "pmtiles",
                "compass",
                "polyline",
                "mercator",
                "easter",
                "beaufort",
            ),
        ),
        (
            "statistics",
            (
                "stats",
                "bloom",
                "hyperloglog",
                "count_min",
                "sketches",
                "dist",
                "hypothesis",
                "decimal",
                "fft",
            ),
        ),
        (
            "math",
            (
                "math",
                "bignum",
                "linalg",
                "numeric",
                "number-theory",
                "ieee754",
                "uint",
                "radix",
                "numfmt",
                "unitconv",
            ),
        ),
        (
            "media",
            (
                "image-meta",
                "exif",
                "color",
                "csscolor",
                "qrcode",
                "pdf-meta",
                "id3",
            ),
        ),
        (
            "time",
            ("chrono", "time", "cron"),
        ),
        (
            "vtab",
            (
                "csv",
                "excel",
                "zipfile",
                "completion",
                "trie",
                "vec",
                "vec0",
                "vec_each",
                "listargs",
                "totype",
                "series",
                "fts5",
                "sqlparse",
                "inmem",
                "define",
                "time-series",
                "arrow",
                "avro",
                "parquet",
                "onnx",
                "container",
            ),
        ),
        (
            "data-structures",
            ("roaring", "skip", "lru"),
        ),
        (
            "utility",
            (
                "closure",
                "eval",
                "fileio",
                "template",
                "currency",
                "country",
                "db-utils",
                "detect",
                "semver",
                "setops",
                "humansize",
                "extfns",
                "parsers",
                "web-parsers",
                "changeset",
            ),
        ),
    ]
    for cat, keys in buckets:
        if n in keys or any(k in n for k in keys):
            return [cat]
    return ["utility"]


_STOPWORDS = {
    "the", "and", "for", "with", "via", "from", "into", "this", "that",
    "are", "but", "any", "not", "all", "out", "one", "two", "see", "can",
    "ext", "sql", "sqlite", "extension", "functions", "function",
}


def derive_keywords(name: str, description: str | None) -> list[str]:
    """Synthesize a small keyword list from the name + first sentence.

    Stop-words filtered so the resulting list is search-useful. Keeps
    the extension's own name as the first keyword.
    """
    out = [name.lower()]
    seen = set(out)
    if description:
        first = description.split(".")[0].lower()
        for tok in first.split():
            tok = "".join(c for c in tok if c.isalpha())
            if not (3 <= len(tok) <= 20):
                continue
            if tok in _STOPWORDS or tok in seen:
                continue
            seen.add(tok)
            out.append(tok)
            if len(out) >= 6:
                break
    return out


def trim_description(description: str | None, max_chars: int = 280) -> str:
    """Take the first sentence / paragraph of a long crate-level doc.

    Cargo.toml descriptions can be multi-paragraph; the registry's
    description field is a short search-result blurb. Strip leading
    whitespace, take the first paragraph, cap to max_chars.
    """
    if not description:
        return ""
    # First paragraph (double-newline OR first sentence)
    para = description.strip().split("\n\n", 1)[0].strip()
    # Collapse whitespace
    para = " ".join(para.split())
    # First sentence if long
    if len(para) > max_chars:
        period = para.rfind(".", 0, max_chars)
        if period > max_chars // 2:
            para = para[: period + 1]
        else:
            para = para[: max_chars].rstrip() + ""
    return para


def artifact_url(base: str, name: str, version: str) -> str:
    """Compose the URL where the built `.component.wasm` lives.

    Resolves to /asset/<name>  the route served by the in-DB CAS
    in extensions-site/registry.db. Empty `base` keeps the URL
    relative so the page works regardless of deployed hostname;
    set `base` (an absolute https URL) when generating an installer-
    facing index.json that needs absolute URLs."""
    base = base.rstrip("/")
    return f"{base}/asset/{name}"


def build_entry(
    conn: sqlite3.Connection,
    plugin_row: sqlite3.Row,
    repo: Path,
    artifact_base: str,
) -> dict:
    """Build a single registry entry from a `plugin_latest` row."""
    name = plugin_row["plugin"]
    version = plugin_row["version"] or "0.1.0"
    description = (
        trim_description(plugin_row["description"])
        or f"SQLite extension: {name}"
    )
    license_ = plugin_row["license"] or "MIT"
    authors_field = plugin_row["authors"] or ""
    authors = [a.strip() for a in authors_field.split(";") if a.strip()]
    if not authors:
        authors = ["sqlite-wasm contributors"]

    # Exports: distinct scalar/aggregate names + vtab table names.
    exports_cur = conn.execute(
        """
        SELECT DISTINCT f.name
        FROM sql_surface f
        WHERE f.plugin = ? AND f.version = ?
        ORDER BY f.name
        """,
        (name, version),
    )
    exports = [r[0] for r in exports_cur]

    # Artifact: pick the component-wasm if present, else core-wasm.
    art_cur = conn.execute(
        """
        SELECT artifact_kind, sha256, size_bytes
        FROM artifact_index
        WHERE plugin = ? AND version = ?
        ORDER BY CASE artifact_kind WHEN 'component-wasm' THEN 0 ELSE 1 END,
                 built_at DESC
        LIMIT 1
        """,
        (name, version),
    )
    artifact = art_cur.fetchone()
    checksum = f"sha256:{artifact[1]}" if artifact else "sha256:unbuilt"
    size_bytes = artifact[2] if artifact else 0

    # Dependencies: extract crates.io path-style deps from the dep_graph.
    deps_cur = conn.execute(
        """
        SELECT DISTINCT dep_name, version_req
        FROM dep_graph
        WHERE plugin = ? AND plugin_version = ? AND source = 'crates.io'
              AND optional = 0
              AND dep_name NOT IN ('wit-bindgen', 'wit-bindgen-rt',
                                   'libsqlite3-sys', 'sqlite-embed',
                                   'sqlite-wasm-core')
        ORDER BY dep_name
        """,
        (name, version),
    )
    dependencies = [f"{d[0]}@{d[1] or 'latest'}" for d in deps_cur]

    return {
        "name": name,
        "version": version,
        "description": description,
        "license": license_,
        "authors": authors,
        "repository": "https://github.com/anthropics/sqlite-wasm",
        "homepage": f"https://github.com/anthropics/sqlite-wasm/tree/main/extensions/{name}",
        "keywords": derive_keywords(name, description),
        "categories": categorize(name, plugin_row["declared_world"]),
        "source": "builtin",
        "artifact_url": artifact_url(artifact_base, name, version),
        "checksum": checksum,
        "size_bytes": size_bytes,
        "min_sqlite_version": MIN_SQLITE_VERSION,
        "exports": exports,
        "dependencies": dependencies,
    }


def build_registry(db_path: Path, repo: Path, artifact_base: str) -> dict:
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        """
        SELECT * FROM plugin_latest
        ORDER BY plugin
        """
    ).fetchall()

    # Keep the extension-manager pseudo-entry at the top  it's the
    # consumer of the registry, not a real entry in the DB, but the
    # `tools/sqlite-wasm-ext` CLI expects it as the first record.
    entries = [
        {
            "name": "extension-manager",
            "version": "0.1.0",
            "description": "SQL interface for managing SQLite WASM extensions",
            "license": "MIT",
            "authors": ["SQLite WASM Team"],
            "repository": "https://github.com/anthropics/sqlite-wasm",
            "homepage": "https://github.com/anthropics/sqlite-wasm#readme",
            "keywords": ["extension", "manager", "registry", "install"],
            "categories": ["utility"],
            "source": "builtin",
            "oci_artifact": "ghcr.io/anthropics/sqlite-wasm-extensions/extension-manager:0.1.0",
            "checksum": "sha256:builtin",
            "size_bytes": 0,
            "min_sqlite_version": MIN_SQLITE_VERSION,
            "exports": [
                "wasm_sync",
                "wasm_search",
                "wasm_list",
                "wasm_install",
                "wasm_uninstall",
                "wasm_info",
                "wasm_update",
                "wasm_registry_version",
            ],
            "dependencies": [],
        }
    ]

    for row in rows:
        entries.append(build_entry(conn, row, repo, artifact_base))

    sha = commit_sha(repo)
    return {
        "version": REGISTRY_VERSION,
        "updated": datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0)
        .strftime("%Y-%m-%dT%H:%M:%SZ"),
        "registry_url": REGISTRY_URL,
        "commit_sha": sha,
        "extensions": entries,
    }


def build_candidates(db_path: Path, repo: Path) -> dict:
    """Build the sibling candidates.json from plugin_candidate.

    Same wrapper shape as the registry  version + updated +
    candidates list  so the registry-serving handler can treat
    both files identically.
    """
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        """
        SELECT name, source, description, upstream_url, track, status,
               reason, proposed_crate, added_at, notes
        FROM plugin_candidate
        ORDER BY track, name
        """
    ).fetchall()
    candidates = []
    for r in rows:
        candidates.append(
            {
                "name": r["name"],
                "source": r["source"],
                "description": r["description"],
                "upstream_url": r["upstream_url"],
                "track": r["track"],
                "status": r["status"],
                "reason": r["reason"],
                "proposed_crate": r["proposed_crate"],
                "notes": r["notes"],
            }
        )
    return {
        "version": REGISTRY_VERSION,
        "updated": datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0)
        .strftime("%Y-%m-%dT%H:%M:%SZ"),
        "commit_sha": commit_sha(repo),
        "candidates": candidates,
    }


def main() -> int:
    import os

    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", default="provenance/extensions.db")
    ap.add_argument("--out", default="registry/index.json")
    ap.add_argument("--candidates-out", default="registry/candidates.json")
    ap.add_argument(
        "--artifact-base",
        default=os.environ.get("SQLITE_WASM_ARTIFACT_BASE", DEFAULT_ARTIFACT_BASE),
        help=(
            "Base URL for `.component.wasm` artifacts. "
            "Composed into artifact_url as "
            "<base>/extensions/<name>/<name>-<version>.component.wasm. "
            "Override with $SQLITE_WASM_ARTIFACT_BASE in CI. "
            f"Default: {DEFAULT_ARTIFACT_BASE}"
        ),
    )
    args = ap.parse_args()

    here = Path(__file__).resolve().parent
    repo = here.parent

    db_path = (repo / args.db) if not Path(args.db).is_absolute() else Path(args.db)
    out_path = (repo / args.out) if not Path(args.out).is_absolute() else Path(args.out)
    cand_path = (
        (repo / args.candidates_out)
        if not Path(args.candidates_out).is_absolute()
        else Path(args.candidates_out)
    )

    if not db_path.exists():
        print(
            f"build_registry: db not found at {db_path}  run provenance/scan.py first",
            file=sys.stderr,
        )
        return 1

    payload = build_registry(db_path, repo, args.artifact_base)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(payload, indent=2) + "\n")
    print(
        f"wrote {out_path.relative_to(repo)}  {len(payload['extensions'])} extensions"
    )

    cand_payload = build_candidates(db_path, repo)
    cand_path.parent.mkdir(parents=True, exist_ok=True)
    cand_path.write_text(json.dumps(cand_payload, indent=2) + "\n")
    print(
        f"wrote {cand_path.relative_to(repo)}  {len(cand_payload['candidates'])} candidates"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
