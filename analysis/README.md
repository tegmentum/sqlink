# Function gap analysis  6 DBs

Snapshot from 2026-06-20 comparing SQL function catalogues from
six reference databases against what SQLink ships. Drives the
"what to port next" decisions for the extension survey.

## Sources

`gap-inventories.json` (572 KB): full per-DB function listings,
one block per database. Hand-curated from each DB's vendor docs.

| DB                       | Functions | Source                       |
|--------------------------|-----------|------------------------------|
| postgresql (PG 17)       | 293       | postgresql.org chapter 9     |
| mysql                    | 295       | dev.mysql.com 8.x reference  |
| mariadb                  | 175       | mariadb.com knowledge base   |
| duckdb                   | 274       | duckdb.org functions docs    |
| clickhouse               | 650       | clickhouse.com functions ref |
| snowflake_bigquery       | 385       | combined snowflake + bq      |

Each record carries `name`, `category`, `arity`, `summary` and
optional `aliases`. Operators excluded; aliases collapsed to a
canonical entry. See the `notes` field on each DB block for what
was specifically in/out of scope.

`gap-report.json` (64 KB): the ranked-by-coverage diff. The
`gap_summary` array holds 146 distinct function names that appear
in 2 of 6 source DBs but were NOT in SQLink's catalogue when
the snapshot was taken. Each entry includes `dbs_have_it`,
`dbs_count`, `rough_summary`, `arity`, and `suggested_extension`
(which existing or proposed extension would host the port).

## Status (as of 2026-06-21)

**141 / 146 (97%) of identified gaps are closed.** The survey
work driven by this report shipped through tasks #343-#347
(stdsql, sys-compat, list, stats extension, chrono extension)
and the pre-existing text-utils / math extensions.

5 entries remain and are all dispositioned `skip` or `core`:

| Function   | DBs | Disposition                                       |
|------------|----:|---------------------------------------------------|
| `cast`     |   4 | covered: CAST keyword (SQLite has it)             |
| `decode`   |   4 | ambiguous (PG bytea vs MySQL legacy crypto)       |
| `convert`  |   3 | ambiguous (charset vs type conversion)            |
| `encode`   |   3 | ambiguous (PG bytea vs string->bytes)             |
| `grouping` |   2 | core SQLite: needs `GROUPING SETS` support first  |

`cast` is functionally covered by the SQL keyword.
`decode`/`encode`/`convert` would each need a vendor-specific
extension to disambiguate semantics  not worth shipping a
generic name that doesn't match either expectation.
`grouping` requires `GROUPING SETS` / `ROLLUP` / `CUBE` in the
parser, which SQLite doesn't have today.

## Regenerating

The inventories are hand-curated; there's no scraper to re-run.
To refresh after a meaningful upstream release:

  1. Edit `gap-inventories.json` directly to add / update a DB's
     `functions` array.
  2. Regenerate `gap-report.json` by intersecting against
     `provenance/extensions.db`'s `sql_function` table:

```python
import json, sqlite3
inv = json.load(open("analysis/gap-inventories.json"))
con = sqlite3.connect("provenance/extensions.db")
have = {r[0].lower() for r in con.execute("SELECT DISTINCT lower(name) FROM sql_function")}

# For each function across all DBs, count how many DBs include it.
from collections import defaultdict
db_of = defaultdict(set)
meta = {}
for d in inv:
    for f in d["functions"]:
        nm = f["name"].lower()
        db_of[nm].add(d["database"])
        meta.setdefault(nm, f)

gap_summary = []
for nm, dbs in db_of.items():
    if nm in have or len(dbs) < 2: continue
    f = meta[nm]
    gap_summary.append({
        "function_name": nm,
        "category": f.get("category", ""),
        "dbs_have_it": sorted(dbs),
        "dbs_count": len(dbs),
        "rough_summary": f.get("summary", ""),
        "suggested_extension": "TBD",
        "arity": f.get("arity", ""),
    })
gap_summary.sort(key=lambda g: (-g["dbs_count"], g["function_name"]))
json.dump({"gap_summary": gap_summary}, open("analysis/gap-report.json", "w"), indent=2)
```

The `suggested_extension` field is curated manually  it expresses
intent ("this belongs in stats", "stub in sys-compat") rather than
existence. After regenerating, sweep the new entries and assign
each one.

## Next round

When this is re-run, expected additions:
  - PostgreSQL 18 (Sept 2025) added a handful of bytea + jsonb
    helpers; revisit the json1 extension's surface.
  - DuckDB ships a fast catalogue; quarterly resync is realistic.
  - ClickHouse adds analytic functions aggressively  always
    bumps the 650-count higher.

Task #338 closes here. Future refreshes can land as their own
small tasks (regenerate + diff + port the new fan-out-of-3+
entries).
