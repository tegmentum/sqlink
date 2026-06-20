#!/usr/bin/env python3
"""Build extensions-site/registry.db  the SQLite-driven extension
registry site, served by sqlite-wasm-httpd.

Reads:
  provenance/extensions.db   (canonical shipped catalog)
  registry/index.json        (rendered registry entries with checksums + exports)
  registry/candidates.json   (planned + deferred/blocked/skipped items)
  extensions-site/templates/ (index.html + ext.html + style.css)

Writes:
  extensions-site/registry.db

The output DB contains the `routes` table sqlite-wasm-httpd needs +
a denormalized `extensions` table powering the page handlers. Routes
are SQL handlers that concat HTML using values from `extensions`.

Run:
    python3 extensions-site/build.py

Then serve locally:
    ./target/release/sqlite-wasm-httpd --db extensions-site/registry.db --port 8080
    open http://localhost:8080
"""

from __future__ import annotations

import argparse
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


def render_card_data(shipped: list[dict], candidates: list[dict]) -> list[dict]:
    """Produce the lightweight per-entry JSON the landing page filters
    over. Keep this slim  the full payload is the per-extension page."""
    out = []
    for e in shipped:
        # Skip the extension-manager pseudo-entry  it's an artifact
        # of the cli registry consumer, not a real extension.
        if e.get("name") == "extension-manager":
            continue
        out.append(
            {
                "name": e["name"],
                "state": "shipped",
                "version": e.get("version", ""),
                "description": e.get("description", "")[:240],
                "categories": e.get("categories", []) or [],
                "keywords": e.get("keywords", []) or [],
                "exports_count": len(e.get("exports", []) or []),
            }
        )
    for c in candidates:
        out.append(
            {
                "name": c["name"],
                "state": c.get("status", "planned"),
                "track": c.get("track", ""),
                "description": (c.get("description") or "")[:240],
                "proposed_crate": c.get("proposed_crate", ""),
                "keywords": [],
                "categories": [c.get("track", "")] if c.get("track") else [],
            }
        )
    # Stable sort: shipped first, then candidates; by name within each group.
    state_order = {"shipped": 0, "planned": 1, "deferred": 2, "blocked": 3, "skipped": 4}
    out.sort(key=lambda e: (state_order.get(e["state"], 9), e["name"]))
    return out


def sql_str(s: str) -> str:
    """Wrap a Python string as a SQL literal. Single-quote escape only.
    Used inside handler bodies to bake static HTML chunks."""
    return "'" + s.replace("'", "''") + "'"


def install_routes(conn: sqlite3.Connection) -> None:
    """The sqlite-wasm-httpd routes table (per its --init-routes
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


def install_extensions_table(
    conn: sqlite3.Connection, cards: list[dict], shipped_full: list[dict], candidates_full: list[dict]
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
            min_sqlite_version TEXT
        )
        """
    )
    by_name_ship = {e["name"]: e for e in shipped_full}
    by_name_cand = {c["name"]: c for c in candidates_full}

    for c in cards:
        name = c["name"]
        if c["state"] == "shipped":
            e = by_name_ship[name]
            description = e.get("description") or ""
            conn.execute(
                """
                INSERT INTO extensions VALUES
                (?, 'shipped', ?, ?, ?, ?, ?, NULL, ?, ?, NULL, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                    e.get("repository", ""),
                    e.get("homepage", ""),
                    e.get("repository", ""),
                    e.get("artifact_url", ""),
                    e.get("checksum", ""),
                    e.get("size_bytes", 0),
                    json.dumps(e.get("exports", [])),
                    json.dumps(e.get("dependencies", [])),
                    e.get("min_sqlite_version", ""),
                ),
            )
        else:
            cd = by_name_cand[name]
            description = cd.get("description") or ""
            conn.execute(
                """
                INSERT INTO extensions VALUES
                (?, ?, NULL, ?, ?, NULL, NULL, ?, ?, '[]', ?, ?, ?, NULL, NULL, NULL, NULL, NULL, '[]', '[]', NULL)
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

    The whole HTML body is built as one giant SQL expression
    one constant string with placeholder substitutions filled in
    by ||-concat. Cheapest interpretation: it's a print statement
    with a few dynamic counts. Cards are JSON-encoded in a <script>
    tag for client-side filtering."""
    shipped_count = sum(1 for c in cards if c["state"] == "shipped")
    planned_count = len(cards) - shipped_count
    total = len(cards)
    data_json = json.dumps(cards, separators=(",", ":"))

    page = (
        template.replace("{{STYLE}}", style)
        .replace("{{TOTAL_COUNT}}", str(total))
        .replace("{{SHIPPED_COUNT}}", str(shipped_count))
        .replace("{{PLANNED_COUNT}}", str(planned_count))
        .replace("{{DATA_JSON}}", data_json)
    )
    # Encode the literal HTML as a SQL string. The handler returns
    # exactly that string in the `body` column.
    return f"SELECT {sql_str(page)} AS body, 'text/html; charset=utf-8' AS ctype"


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
              replace(replace(replace(replace(replace(replace(replace(replace(replace(
                {sql_str(skel)},
                '{{{{NAME}}}}',          (SELECT name FROM e)),
                '{{{{DESC_META}}}}',     (SELECT replace(replace(coalesce(description, ''), '"', '&quot;'), char(10), ' ') FROM e)),
                '{{{{STATE}}}}',         (SELECT state FROM e)),
                '{{{{DESCRIPTION}}}}',   (SELECT coalesce(description_html, '') FROM e)),
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
                    CASE WHEN repository IS NOT NULL AND repository != '' THEN '<dt>source</dt><dd><a href="' || repository || '">' || repository || '</a></dd>' ELSE '' END ||
                    CASE WHEN min_sqlite_version IS NOT NULL AND min_sqlite_version != '' THEN '<dt>min sqlite</dt><dd>' || min_sqlite_version || '</dd>' ELSE '' END
                    FROM e)),
                '{{{{EXPORTS_SECTION}}}}',
                  (SELECT CASE WHEN exports_json IS NOT NULL AND exports_json != '[]'
                    THEN '<div class="section"><h3>Functions ('
                         || (SELECT COUNT(*) FROM json_each(exports_json))
                         || ')</h3><ul class="export-list">'
                         || (SELECT group_concat('<li>' || value || '</li>', '') FROM json_each(exports_json))
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
              'upstream_url', upstream_url
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
                'upstream_url', upstream_url
              ) FROM e)
          END AS body,
          CASE WHEN (SELECT COUNT(*) FROM e) = 0 THEN 404 ELSE 200 END AS status,
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
    cards = render_card_data(shipped, candidates)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    if out_path.exists():
        out_path.unlink()

    conn = sqlite3.connect(out_path)
    conn.execute("PRAGMA journal_mode = DELETE")
    install_routes(conn)
    install_extensions_table(conn, cards, shipped, candidates)
    install_route_rows(conn, cards)

    # Sanity index for the per-extension lookup path.
    conn.execute("CREATE UNIQUE INDEX extensions_name ON extensions(name)")
    conn.commit()

    size = out_path.stat().st_size
    print(
        f"wrote {out_path.relative_to(REPO_ROOT)}  "
        f"{len(cards)} extensions, {size:,} bytes"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
