#!/usr/bin/env python3
"""Build extensions-site/registry.db  the SQLite-driven extension
registry site, served by sqlink-httpd.

Reads:
  provenance/extensions.db   (canonical shipped catalog)
  registry/index.json        (rendered registry entries with checksums + exports)
  registry/candidates.json   (planned + deferred/blocked/skipped items)
  extensions-site/templates/ (index.html + ext.html + style.css)

Writes:
  extensions-site/registry.db

The output DB contains the `routes` table sqlink-httpd needs +
a denormalized `extensions` table powering the page handlers. Routes
are SQL handlers that concat HTML using values from `extensions`.

Run:
    python3 extensions-site/build.py

Then serve locally:
    ./target/release/sqlink-httpd --db extensions-site/registry.db --port 8080
    open http://localhost:8080
"""

from __future__ import annotations

import argparse
import hashlib
import html
import json
import sqlite3
import sys
import textwrap
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SITE_ROOT = REPO_ROOT / "extensions-site"
TEMPLATES = SITE_ROOT / "templates"


def load_data(repo: Path) -> tuple[list[dict], list[dict]]:
    """Load shipped + candidate lists with the fields the site needs."""
    index_path = repo / "registry" / "index.json"
    candidates_path = repo / "registry" / "candidates.json"
    if not index_path.exists() or not candidates_path.exists():
        sys.exit(
            f"registry/index.json or candidates.json missing  run\n"
            f"  python3 provenance/scan.py && python3 provenance/build_registry.py\n"
            f"first."
        )
    index = json.loads(index_path.read_text())
    candidates = json.loads(candidates_path.read_text())
    return index["extensions"], candidates["candidates"]


def render_card_data(
    shipped: list[dict], kinds_map: dict, ext_dbs_map: dict
) -> list[dict]:
    """Produce the lightweight per-entry JSON the landing page filters
    over. Includes the rolled-up type tags so the cards can show
    [UDF] / [UDAF] / [UDTF] / [collation] chips and the search bar
    can match on them.

    ext_dbs_map carries the per-extension roll-up of source DBs (PG /
    MySQL / etc.)  any DB whose function set overlaps with this
    extension's function names ends up in the card's `source_dbs`
    list, driving the source-DB filter chips."""
    # Cap the per-card exports list so the JSON blob stays small.
    # The detail page shows the full list; the card just teases what's
    # inside. Big extensions (postgis-bridge has 420+) would otherwise
    # blow up the payload.
    CARD_EXPORTS_CAP = 12
    out = []
    for e in shipped:
        # Skip the extension-manager pseudo-entry  it's an artifact
        # of the cli registry consumer, not a real extension.
        if e.get("name") == "extension-manager":
            continue
        name = e["name"]
        exports = e.get("exports", []) or []
        out.append(
            {
                "name": name,
                "state": "shipped",
                "version": e.get("version", ""),
                "description": e.get("description", "")[:240],
                "categories": e.get("categories", []) or [],
                "keywords": e.get("keywords", []) or [],
                "exports_count": len(exports),
                "exports_preview": list(exports[:CARD_EXPORTS_CAP]),
                "tags": kinds_map.get(name, []),
                "source_dbs": ext_dbs_map.get(name, []),
            }
        )
    out.sort(key=lambda e: e["name"])
    return out


def sql_str(s: str) -> str:
    """Wrap a Python string as a SQL literal. Single-quote escape only.
    Used inside handler bodies to bake static HTML chunks."""
    return "'" + s.replace("'", "''") + "'"


def install_routes(conn: sqlite3.Connection) -> None:
    """The sqlink-httpd routes table (per its --init-routes
    schema) drives every request. Each row is one route; handler
    text is SQL that builds the response body. Built-in /sql and
    /tables endpoints from the httpd binary take precedence over
    db-driven routes  we deliberately don't shadow them."""
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS routes (
            method   TEXT NOT NULL,
            pattern  TEXT NOT NULL,
            handler  TEXT NOT NULL,
            kind     TEXT NOT NULL DEFAULT 'sql',
            status   INTEGER DEFAULT 200,
            ctype    TEXT,
            priority INTEGER DEFAULT 0
        )
        """
    )
    conn.execute("DELETE FROM routes")


def install_static_assets(conn: sqlite3.Connection, site_root: Path) -> int:
    """Site chrome  logo, favicon, anything else served at /static/<name>.
    Distinct from the per-extension assets table so the schemas can
    evolve independently. The httpd's `blob` route kind serves either."""
    conn.execute(
        """
        CREATE TABLE static_assets (
            name         TEXT PRIMARY KEY,
            blob         BLOB NOT NULL,
            content_type TEXT NOT NULL
        )
        """
    )
    files = [
        # (filename in templates/site root, content-type)
        ("sqlink_logo.png", "image/png"),
    ]
    count = 0
    for filename, ctype in files:
        path = site_root / filename
        if not path.exists():
            print(f"warning: static asset {filename} not found at {path}", file=sys.stderr)
            continue
        conn.execute(
            "INSERT INTO static_assets (name, blob, content_type) VALUES (?, ?, ?)",
            (filename, path.read_bytes(), ctype),
        )
        count += 1
    return count


def install_assets_table(conn: sqlite3.Connection, repo: Path) -> tuple[int, int]:
    """Build the content-addressed asset store. Walks every extension's
    `target/wasm32-wasip2/release/<name>_extension.component.wasm` and
    inserts it as a BLOB keyed by extension name. The /asset/<name>
    route serves these directly via the httpd's `blob` route kind
    no R2, no external storage, no DNS, no egress fees.

    Returns (asset_count, total_bytes) for the build-time summary."""
    conn.execute(
        """
        CREATE TABLE assets (
            name         TEXT PRIMARY KEY,
            blob         BLOB NOT NULL,
            size_bytes   INTEGER NOT NULL,
            sha256       TEXT NOT NULL,
            content_type TEXT NOT NULL DEFAULT 'application/wasm',
            version      TEXT
        )
        """
    )
    count = 0
    total = 0
    artifacts = sorted(
        (repo / "extensions").glob(
            "*/target/wasm32-wasip2/release/*_extension.component.wasm"
        )
    )
    for art in artifacts:
        name = art.parts[-5]  # extensions/<name>/target/wasm32-wasip2/release/...
        # Read version from Cargo.toml for the metadata column.
        cargo = repo / "extensions" / name / "Cargo.toml"
        version = "0.1.0"
        if cargo.exists():
            for line in cargo.read_text().splitlines():
                if line.strip().startswith("version") and "=" in line:
                    version = line.split("=", 1)[1].strip().strip('"').strip("'")
                    break
        blob = art.read_bytes()
        sha = hashlib.sha256(blob).hexdigest()
        conn.execute(
            "INSERT INTO assets (name, blob, size_bytes, sha256, content_type, version) "
            "VALUES (?, ?, ?, ?, 'application/wasm', ?)",
            (name, blob, len(blob), sha, version),
        )
        count += 1
        total += len(blob)
    return count, total


# Vtabs that act as a queryable index over their backing data.
# SQLite has no separate "index module" API  index-shaped extensions
# are exposed as virtual tables. This is the curated override that
# adds the `index` tag alongside `UDTF` for those cases.
INDEX_LIKE_VTABS = {
    "vec0",       # kNN over an embeddings column (brute / IVF / HNSW / LSH)
    "fts5",       # full-text inverted index
    "rtree",      # spatial bounding-box index
    "geopoly",    # polygon index (built on rtree)
    "spellfix1",  # fuzzy / phonetic match index
    "trie",       # prefix tree over a TEXT column
}


# Short labels for the six reference DBs from the function-gap
# inventories. Surface lengths come from analysis/gap-inventories.json
# (generated by the parallel-research workflow). We collapse the
# Snowflake/BigQuery bundle into a single chip since the inventory
# itself merges them.
DB_LABELS = {
    "postgresql":         "PostgreSQL",
    "mysql":              "MySQL",
    "mariadb":            "MariaDB",
    "duckdb":             "DuckDB",
    "clickhouse":         "ClickHouse",
    "snowflake_bigquery": "Snowflake/BigQuery",
}


def compute_function_db_origins(inventories_path: Path) -> dict:
    """Read analysis/gap-inventories.json (produced by the gap
    analysis workflow) and return a map keyed by lowercased
    function name → list of short DB labels.

    Each inventory entry has `name` + `aliases`; we attribute the
    DB to the canonical name AND every alias so e.g. `dayofmonth`
    (an alias of `day`) gets MySQL/MariaDB."""
    if not inventories_path.exists():
        return {}
    raw = json.loads(inventories_path.read_text())
    out: dict[str, set[str]] = {}
    for inv in raw:
        db = inv.get("database", "")
        label = DB_LABELS.get(db)
        if not label:
            continue
        for fn in inv.get("functions", []):
            for n in [fn.get("name", "")] + (fn.get("aliases") or []):
                if not n:
                    continue
                out.setdefault(n.lower(), set()).add(label)
    # Stable ordering matches the DB_LABELS dict order so chips
    # render the same way on every page.
    label_order = list(DB_LABELS.values())
    return {
        k: sorted(v, key=lambda lbl: label_order.index(lbl) if lbl in label_order else 99)
        for k, v in out.items()
    }


def compute_extension_db_attribution(
    provenance_db: Path, fn_db_map: dict
) -> tuple[dict, dict]:
    """Two views of the per-extension DB attribution:

      1. {plugin → {function_name → [db_labels]}}   for the detail
         page (each function gets inline DB chips).
      2. {plugin → [db_labels]}                     for the landing
         card / filter (DB chip if any function matches).

    Functions absent from the gap inventories simply have no chips."""
    if not provenance_db.exists():
        return {}, {}
    conn = sqlite3.connect(provenance_db)
    try:
        rows = conn.execute(
            "SELECT DISTINCT plugin, name FROM sql_surface"
        ).fetchall()
    finally:
        conn.close()
    per_fn: dict[str, dict[str, list[str]]] = {}
    per_ext: dict[str, set[str]] = {}
    for plugin, fname in rows:
        dbs = fn_db_map.get((fname or "").lower(), [])
        if not dbs:
            continue
        per_fn.setdefault(plugin, {})[fname] = dbs
        per_ext.setdefault(plugin, set()).update(dbs)
    label_order = list(DB_LABELS.values())
    per_ext_sorted = {
        k: sorted(v, key=lambda lbl: label_order.index(lbl) if lbl in label_order else 99)
        for k, v in per_ext.items()
    }
    return per_fn, per_ext_sorted


def compute_kinds(provenance_db: Path) -> dict:
    """Roll up sql_function.kind to per-extension type tags.

    Returns {name -> [tags...]} where tags are the canonical UDF/
    UDAF/UDTF/collation labels surfaced on the site. Extensions
    with multiple kinds (e.g. `decimal` has both scalars and
    aggregates) get multiple tags. Index-shaped vtabs additionally
    get an `index` tag via INDEX_LIKE_VTABS."""
    if not provenance_db.exists():
        return {}
    conn = sqlite3.connect(provenance_db)
    rows = conn.execute(
        """
        SELECT plugin, group_concat(DISTINCT kind) AS kinds
        FROM sql_surface
        GROUP BY plugin
        """
    ).fetchall()
    label = {
        "scalar":    "UDF",
        "aggregate": "UDAF",
        "vtab":      "UDTF",
        "collation": "collation",
    }
    out = {}
    for plugin, kinds in rows:
        if not kinds:
            continue
        tags = []
        seen = set()
        for k in kinds.split(","):
            lab = label.get(k, k)
            if lab not in seen:
                tags.append(lab)
                seen.add(lab)
        if plugin in INDEX_LIKE_VTABS:
            tags.append("index")
        # Stable ordering: UDF, UDAF, UDTF, index, collation, then anything else.
        order = {"UDF": 0, "UDAF": 1, "UDTF": 2, "index": 3, "collation": 4}
        tags.sort(key=lambda t: order.get(t, 99))
        out[plugin] = tags
    # Pure-vtab index extensions (fts5, rtree, geopoly) have no
    # sql_function rows  the surface is the CREATE VIRTUAL TABLE
    # interface, not enumerable functions. Tag them directly.
    for plugin in INDEX_LIKE_VTABS:
        if plugin not in out:
            out[plugin] = ["UDTF", "index"]
    conn.close()
    return out


def install_extensions_table(
    conn: sqlite3.Connection,
    cards: list[dict],
    shipped_full: list[dict],
    candidates_full: list[dict],
    function_dbs_map: dict | None = None,
    extension_dbs_map: dict | None = None,
) -> None:
    """Denormalized per-extension table. Each row carries the SQL
    handlers' inputs in pre-computed form so the handler text is
    a constant query against a real row rather than a tangle of
    json_extract calls."""
    conn.execute(
        """
        CREATE TABLE extensions (
            name              TEXT PRIMARY KEY,
            state             TEXT NOT NULL,
            version           TEXT,
            description       TEXT,
            description_html  TEXT,
            license           TEXT,
            authors_json      TEXT,
            track             TEXT,
            categories_json   TEXT,
            keywords_json     TEXT,
            tags_json         TEXT,
            proposed_crate    TEXT,
            reason            TEXT,
            upstream_url      TEXT,
            homepage          TEXT,
            repository        TEXT,
            artifact_url      TEXT,
            checksum          TEXT,
            size_bytes        INTEGER,
            exports_json      TEXT,
            dependencies_json TEXT,
            min_sqlite_version TEXT,
            function_dbs_json  TEXT,
            source_dbs_json    TEXT
        )
        """
    )
    by_name_ship = {e["name"]: e for e in shipped_full}
    by_name_cand = {c["name"]: c for c in candidates_full}

    for c in cards:
        name = c["name"]
        if c["state"] == "shipped":
            e = by_name_ship[name]
            # Card already carries the rolled-up tags; flow them into
            # the per-extension row so the detail handler can render
            # them without re-querying provenance.
            e["tags"] = c.get("tags", [])
            description = e.get("description") or ""
            # Column order: name, state, version, description, description_html,
            # license, authors_json, track, categories_json, keywords_json,
            # proposed_crate, reason, upstream_url, homepage, repository,
            # artifact_url, checksum, size_bytes, exports_json, dependencies_json,
            # min_sqlite_version.
            # upstream_url is the per-extension RFC / spec / reference impl
            # link, sourced from provenance/upstream-urls.json by scan.py
            # and flowed through registry/index.json by build_registry.py.
            # Missing here  the detail page hides the "upstream" row.
            conn.execute(
                """
                INSERT INTO extensions VALUES
                (?, 'shipped', ?, ?, ?, ?, ?, NULL, ?, ?, ?, NULL, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    name,
                    e.get("version", ""),
                    description,
                    html.escape(description),
                    e.get("license", ""),
                    json.dumps(e.get("authors", [])),
                    json.dumps(e.get("categories", [])),
                    json.dumps(e.get("keywords", [])),
                    json.dumps(e.get("tags", [])),
                    e.get("upstream_url") or None,
                    e.get("homepage", ""),
                    e.get("repository", ""),
                    e.get("artifact_url", ""),
                    e.get("checksum", ""),
                    e.get("size_bytes", 0),
                    json.dumps(e.get("exports", [])),
                    json.dumps(e.get("dependencies", [])),
                    e.get("min_sqlite_version", ""),
                    json.dumps((function_dbs_map or {}).get(name, {})),
                    json.dumps((extension_dbs_map or {}).get(name, [])),
                ),
            )
        else:
            cd = by_name_cand[name]
            description = cd.get("description") or ""
            conn.execute(
                """
                INSERT INTO extensions VALUES
                (?, ?, NULL, ?, ?, NULL, NULL, ?, ?, '[]', ?, ?, ?, NULL, NULL, NULL, NULL, NULL, '[]', '[]', NULL, '{}', '[]')
                """,
                (
                    name,
                    c["state"],
                    description,
                    html.escape(description),
                    cd.get("track", ""),
                    json.dumps([cd.get("track", "")] if cd.get("track") else []),
                    cd.get("proposed_crate", ""),
                    cd.get("reason", ""),
                    cd.get("upstream_url", ""),
                ),
            )


def landing_handler(template: str, style: str, cards: list[dict]) -> str:
    """Return the SQL handler text that produces the landing page.

    The HTML body is built as a SQL expression. Counts (extensions
    + distinct SQL functions) are derived AT REQUEST TIME via
    subqueries against the served registry  so the displayed
    numbers always match the live db rather than baking in a
    Python int at site-build time.

    Cards are JSON-encoded in a <script> tag for client-side
    filtering; the array length client-side mirrors the SQL count
    as a cross-check."""
    data_json = json.dumps(cards, separators=(",", ":"))

    # Pre-fill static placeholders, then split on the dynamic ones
    # so we can ||-concat live subqueries at the right spots.
    page = (
        template.replace("{{STYLE}}", style)
        .replace("{{DATA_JSON}}", data_json)
    )
    EXT_PLACEHOLDER = "{{EXT_COUNT}}"
    FN_PLACEHOLDER = "{{FN_COUNT}}"
    # Sub each {{TOTAL_COUNT}} occurrence with the EXT placeholder
    # the template currently uses the TOTAL_COUNT name for the
    # extension count; rename here so the SQL split is unambiguous.
    page = page.replace("{{TOTAL_COUNT}}", EXT_PLACEHOLDER)

    parts: list[str] = []
    cursor = page
    while True:
        # Find whichever placeholder comes first.
        next_ext = cursor.find(EXT_PLACEHOLDER)
        next_fn = cursor.find(FN_PLACEHOLDER)
        candidates = [(i, k) for i, k in [(next_ext, "ext"), (next_fn, "fn")] if i >= 0]
        if not candidates:
            parts.append(sql_str(cursor))
            break
        idx, which = min(candidates, key=lambda x: x[0])
        parts.append(sql_str(cursor[:idx]))
        if which == "ext":
            parts.append("(SELECT count FROM site_meta WHERE key='extension_count')")
            cursor = cursor[idx + len(EXT_PLACEHOLDER):]
        else:
            parts.append("(SELECT count FROM site_meta WHERE key='function_count')")
            cursor = cursor[idx + len(FN_PLACEHOLDER):]

    body_expr = " || ".join(parts)
    return f"SELECT {body_expr} AS body, 'text/html; charset=utf-8' AS ctype"


def install_site_meta(
    conn: sqlite3.Connection, provenance_db: Path, cards: list[dict]
) -> None:
    """Aggregate counts live in their own table so the landing
    handler can pull them at request time. Single source of truth:
    provenance/extensions.db for the function count; the served
    registry for the extension count (which the cards built).

    Rationale: baking counts into a Python int at build time made
    the subtitle go stale relative to the served db if anyone
    updated registry.db separately. Storing in a table keeps the
    numbers self-consistent."""
    conn.execute("CREATE TABLE site_meta (key TEXT PRIMARY KEY, count INTEGER NOT NULL)")
    ext_count = len(cards)
    fn_count = 0
    if provenance_db.exists():
        prov = sqlite3.connect(provenance_db)
        try:
            row = prov.execute(
                "SELECT COUNT(DISTINCT name) FROM sql_surface"
            ).fetchone()
            fn_count = row[0] if row else 0
        finally:
            prov.close()
    conn.executemany(
        "INSERT INTO site_meta (key, count) VALUES (?, ?)",
        [("extension_count", ext_count), ("function_count", fn_count)],
    )


def detail_handler(template: str, style: str) -> str:
    """SQL handler for /ext/*. Parses the extension name out of :path
    then renders the per-extension template with the matched row's
    columns."""
    # The pattern is /ext/*; the dispatcher passes the full path as
    # :path. We strip the prefix in SQL.
    skel = template.replace("{{STYLE}}", style)
    # Per-cell renders. SQL's ||-concat + json_each iteration gets
    # awkward; cleaner to keep these mostly static and patch with
    # || at the seam points.
    body_sql = textwrap.dedent(
        f"""
        WITH req AS (
            SELECT substr(:path, length('/ext/') + 1) AS ext_name
        ),
        e AS (
            SELECT * FROM extensions
            WHERE name = (SELECT ext_name FROM req)
        )
        SELECT
          CASE WHEN (SELECT COUNT(*) FROM e) = 0
            THEN '<!DOCTYPE html><html><head><title>not found</title><style>'
                 || {sql_str(style)}
                 || '</style></head><body><div class="container"><h1>not found</h1>'
                 || '<p>No extension named <code>'
                 || coalesce((SELECT ext_name FROM req), '')
                 || '</code> in the registry.</p>'
                 || '<p><a href="/">all extensions</a></p></div></body></html>'
            ELSE
              replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(
                {sql_str(skel)},
                '{{{{NAME}}}}',          (SELECT name FROM e)),
                '{{{{DESC_META}}}}',     (SELECT replace(replace(coalesce(description, ''), '"', '&quot;'), char(10), ' ') FROM e)),
                '{{{{STATE}}}}',         (SELECT state FROM e)),
                '{{{{DESCRIPTION}}}}',   (SELECT coalesce(description_html, '') FROM e)),
                '{{{{TAGS_HTML}}}}',
                  (SELECT CASE WHEN tags_json IS NOT NULL AND tags_json != '[]'
                    THEN (SELECT group_concat(
                           '<span class="tag tag-' || lower(value) || '">' || value || '</span>',
                           '') FROM json_each(tags_json))
                    ELSE '' END FROM e)),
                '{{{{ACTIONS}}}}',
                  (SELECT CASE WHEN state = 'shipped' AND artifact_url != ''
                    THEN '<a class="btn" href="' || artifact_url || '">Download artifact</a>'
                         || CASE WHEN upstream_url != '' THEN '<a class="btn secondary" href="' || upstream_url || '">Source</a>' ELSE '' END
                    ELSE
                      CASE WHEN upstream_url != ''
                        THEN '<a class="btn secondary" href="' || upstream_url || '">Reference</a>'
                        ELSE ''
                      END
                    END FROM e)),
                '{{{{DETAIL_ROWS}}}}',
                  (SELECT
                    CASE WHEN version IS NOT NULL AND version != '' THEN '<dt>version</dt><dd>' || version || '</dd>' ELSE '' END ||
                    CASE WHEN license IS NOT NULL AND license != '' THEN '<dt>license</dt><dd>' || license || '</dd>' ELSE '' END ||
                    CASE WHEN track IS NOT NULL AND track != '' THEN '<dt>track</dt><dd>' || track || '</dd>' ELSE '' END ||
                    CASE WHEN categories_json IS NOT NULL AND categories_json != '[]'
                      THEN '<dt>categories</dt><dd>' ||
                           (SELECT group_concat(value, ', ') FROM json_each(categories_json))
                           || '</dd>' ELSE '' END ||
                    CASE WHEN keywords_json IS NOT NULL AND keywords_json != '[]'
                      THEN '<dt>keywords</dt><dd>' ||
                           (SELECT group_concat(value, ', ') FROM json_each(keywords_json))
                           || '</dd>' ELSE '' END ||
                    CASE WHEN checksum IS NOT NULL AND checksum != '' THEN '<dt>checksum</dt><dd>' || checksum || '</dd>' ELSE '' END ||
                    CASE WHEN size_bytes IS NOT NULL AND size_bytes > 0 THEN '<dt>size</dt><dd>' || size_bytes || ' bytes</dd>' ELSE '' END ||
                    CASE WHEN proposed_crate IS NOT NULL AND proposed_crate != '' THEN '<dt>proposed crate</dt><dd>' || proposed_crate || '</dd>' ELSE '' END ||
                    CASE WHEN upstream_url IS NOT NULL AND upstream_url != '' THEN '<dt>upstream</dt><dd><a href="' || upstream_url || '">' || upstream_url || '</a></dd>' ELSE '' END ||
                    CASE WHEN homepage IS NOT NULL AND homepage != '' THEN '<dt>source</dt><dd><a href="' || homepage || '">' || homepage || '</a></dd>' ELSE '' END ||
                    CASE WHEN min_sqlite_version IS NOT NULL AND min_sqlite_version != '' THEN '<dt>min sqlite</dt><dd>' || min_sqlite_version || '</dd>' ELSE '' END
                    FROM e)),
                '{{{{EXPORTS_SECTION}}}}',
                  (SELECT CASE WHEN exports_json IS NOT NULL AND exports_json != '[]'
                    THEN '<div class="section"><h3>Functions ('
                         || (SELECT COUNT(*) FROM json_each(exports_json))
                         || ')</h3>'
                         || CASE WHEN source_dbs_json IS NOT NULL AND source_dbs_json != '[]'
                              THEN '<p class="export-key">Also in: '
                                || (SELECT group_concat('<span class="db db-' || lower(replace(replace(value, '/', '-'), ' ', '-')) || '">' || value || '</span>', ' ') FROM json_each(source_dbs_json))
                                || '</p>'
                              ELSE '' END
                         || '<ul class="export-list">'
                         || (SELECT group_concat(
                                '<li><code>' || ex.value || '</code>'
                                || coalesce(
                                     (SELECT ' ' || group_concat(
                                         '<span class="db db-' || lower(replace(replace(dval.value, '/', '-'), ' ', '-')) || '">' || dval.value || '</span>',
                                         '')
                                      FROM json_each(json_extract(function_dbs_json, '$.' || json_quote(ex.value))) AS dval),
                                     '')
                                || '</li>',
                                '')
                            FROM json_each(exports_json) AS ex)
                         || '</ul></div>'
                    ELSE '' END FROM e)),
                '{{{{DEPS_SECTION}}}}',
                  (SELECT CASE WHEN dependencies_json IS NOT NULL AND dependencies_json != '[]'
                    THEN '<div class="section"><h3>Dependencies</h3><div class="dep-list">'
                         || (SELECT group_concat(value, '<br>') FROM json_each(dependencies_json))
                         || '</div></div>'
                    ELSE '' END FROM e)),
                '{{{{REASON_SECTION}}}}',
                  (SELECT CASE WHEN reason IS NOT NULL AND reason != ''
                    THEN '<div class="section"><h3>Status: ' || state || '</h3><p>' || reason || '</p></div>'
                    ELSE '' END FROM e))
            END AS body,
          CASE WHEN (SELECT COUNT(*) FROM e) = 0 THEN 404 ELSE 200 END AS status,
          'text/html; charset=utf-8' AS ctype
        """
    )
    return body_sql.strip()


def api_list_handler() -> str:
    """JSON API for the full ecosystem  used by anything wanting the
    raw data without HTML chrome."""
    return textwrap.dedent(
        """
        SELECT
          '{"version":"1.0.0","extensions":[' ||
          (SELECT group_concat(
            json_object(
              'name', name,
              'state', state,
              'version', version,
              'description', description,
              'license', license,
              'track', track,
              'categories', json(coalesce(categories_json, '[]')),
              'keywords', json(coalesce(keywords_json, '[]')),
              'exports', json(coalesce(exports_json, '[]')),
              'dependencies', json(coalesce(dependencies_json, '[]')),
              'artifact_url', artifact_url,
              'checksum', checksum,
              'size_bytes', size_bytes,
              'proposed_crate', proposed_crate,
              'reason', reason,
              'upstream_url', upstream_url,
              'source_dbs', json(coalesce(source_dbs_json, '[]')),
              'function_dbs', json(coalesce(function_dbs_json, '{}'))
            ),
            ','
          ) FROM extensions)
          || ']}' AS body,
          'application/json' AS ctype
        """
    ).strip()


def api_one_handler() -> str:
    """JSON API for a single extension. 404 on absent."""
    return textwrap.dedent(
        """
        WITH req AS (
            SELECT
                CASE WHEN substr(:path, -5) = '.json'
                  THEN substr(:path, length('/api/ext/') + 1, length(:path) - length('/api/ext/') - 5)
                  ELSE substr(:path, length('/api/ext/') + 1)
                END AS ext_name
        ),
        e AS (
            SELECT * FROM extensions WHERE name = (SELECT ext_name FROM req)
        )
        SELECT
          CASE WHEN (SELECT COUNT(*) FROM e) = 0
            THEN '{"error":"not found","name":' || json_quote((SELECT ext_name FROM req)) || '}'
            ELSE
              (SELECT json_object(
                'name', name,
                'state', state,
                'version', version,
                'description', description,
                'license', license,
                'track', track,
                'categories', json(coalesce(categories_json, '[]')),
                'keywords', json(coalesce(keywords_json, '[]')),
                'exports', json(coalesce(exports_json, '[]')),
                'dependencies', json(coalesce(dependencies_json, '[]')),
                'artifact_url', artifact_url,
                'checksum', checksum,
                'size_bytes', size_bytes,
                'proposed_crate', proposed_crate,
                'reason', reason,
                'upstream_url', upstream_url,
                'source_dbs', json(coalesce(source_dbs_json, '[]')),
                'function_dbs', json(coalesce(function_dbs_json, '{}'))
              ) FROM e)
          END AS body,
          CASE WHEN (SELECT COUNT(*) FROM e) = 0 THEN 404 ELSE 200 END AS status,
          'application/json' AS ctype
        """
    ).strip()


def asset_handler() -> str:
    """SQL that returns ONE blob value  the .component.wasm bytes
    for the requested extension. The httpd's `blob` route kind
    serves the result raw, bypassing the Value-type roundtrip
    that hex-encodes BLOBs in the `sql` kind."""
    return (
        "SELECT blob FROM assets "
        "WHERE name = substr(:path, length('/asset/') + 1)"
    )


def static_handler() -> str:
    """SQL that returns ONE blob value from the static_assets table
    (logo, favicon, etc). Same `blob` kind, separate table from
    the extension artifact CAS so the schemas can evolve
    independently."""
    return (
        "SELECT blob FROM static_assets "
        "WHERE name = substr(:path, length('/static/') + 1)"
    )


def asset_info_handler() -> str:
    """JSON metadata for an artifact  size, sha256, content-type,
    version. Pairs with the binary served by /asset/<name>."""
    return textwrap.dedent(
        """
        WITH req AS (
            SELECT substr(:path, length('/asset-info/') + 1) AS ext_name
        ),
        a AS (
            SELECT * FROM assets WHERE name = (SELECT ext_name FROM req)
        )
        SELECT
          CASE WHEN (SELECT COUNT(*) FROM a) = 0
            THEN '{"error":"not found","name":' || json_quote((SELECT ext_name FROM req)) || '}'
            ELSE
              (SELECT json_object(
                'name', name,
                'size_bytes', size_bytes,
                'sha256', sha256,
                'content_type', content_type,
                'version', version,
                'url', '/asset/' || name
              ) FROM a)
          END AS body,
          CASE WHEN (SELECT COUNT(*) FROM a) = 0 THEN 404 ELSE 200 END AS status,
          'application/json' AS ctype
        """
    ).strip()


def install_route_rows(conn: sqlite3.Connection, cards: list[dict]) -> None:
    style = (TEMPLATES / "style.css").read_text()
    index = (TEMPLATES / "index.html").read_text()
    ext = (TEMPLATES / "ext.html").read_text()

    rows = [
        # method, pattern, handler, kind, status, ctype, priority
        ("GET", "/", landing_handler(index, style, cards), "sql", 200, None, 10),
        ("GET", "/index.html", landing_handler(index, style, cards), "sql", 200, None, 10),
        ("GET", "/ext/*", detail_handler(ext, style), "sql", 200, None, 10),
        ("GET", "/api/extensions.json", api_list_handler(), "sql", 200, None, 10),
        ("GET", "/api/ext/*", api_one_handler(), "sql", 200, None, 10),
        # /asset/<name>  raw .component.wasm bytes from the in-DB CAS.
        # The `blob` kind bypasses Value-type hex encoding; httpd
        # adds Cache-Control: immutable + ACAO: * automatically.
        ("GET", "/asset/*", asset_handler(), "blob", 200, "application/wasm", 10),
        ("GET", "/asset-info/*", asset_info_handler(), "sql", 200, None, 10),
        # /static/<name>  site chrome (logo, favicon, etc).
        # ctype is hardcoded to image/png for v1; if we need other
        # types later, switch to a per-row ctype via a SQL route
        # that prefixes the body with a content-type header.
        ("GET", "/static/*", static_handler(), "blob", 200, "image/png", 10),
        ("GET", "/health", "ok", "static", 200, "text/plain", 100),
        (
            "GET",
            "/robots.txt",
            "User-agent: *\nAllow: /\n",
            "static",
            200,
            "text/plain",
            100,
        ),
    ]
    conn.executemany(
        "INSERT INTO routes (method, pattern, handler, kind, status, ctype, priority) "
        "VALUES (?, ?, ?, ?, ?, ?, ?)",
        rows,
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", default=str(SITE_ROOT / "registry.db"))
    args = ap.parse_args()

    shipped, candidates = load_data(REPO_ROOT)
    kinds_map = compute_kinds(REPO_ROOT / "provenance" / "extensions.db")
    fn_db_map = compute_function_db_origins(REPO_ROOT / "analysis" / "gap-inventories.json")
    function_dbs_map, extension_dbs_map = compute_extension_db_attribution(
        REPO_ROOT / "provenance" / "extensions.db", fn_db_map
    )
    cards = render_card_data(shipped, kinds_map, extension_dbs_map)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    if out_path.exists():
        out_path.unlink()

    conn = sqlite3.connect(out_path)
    conn.execute("PRAGMA journal_mode = DELETE")
    install_routes(conn)
    install_extensions_table(
        conn, cards, shipped, candidates,
        function_dbs_map=function_dbs_map,
        extension_dbs_map=extension_dbs_map,
    )
    install_site_meta(conn, REPO_ROOT / "provenance" / "extensions.db", cards)
    asset_count, asset_bytes = install_assets_table(conn, REPO_ROOT)
    static_count = install_static_assets(conn, SITE_ROOT)
    install_route_rows(conn, cards)
    _ = static_count  # surfaced through the summary line via the table

    # Sanity index for the per-extension lookup path.
    conn.execute("CREATE UNIQUE INDEX extensions_name ON extensions(name)")
    conn.commit()
    conn.close()

    size = out_path.stat().st_size
    print(
        f"wrote {out_path.relative_to(REPO_ROOT)}  "
        f"{len(cards)} extensions, {asset_count} CAS artifacts "
        f"({asset_bytes:,} bytes binary), {size:,} bytes total DB"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
