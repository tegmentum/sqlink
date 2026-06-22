# PLAN: port sqlite-utils CLI surface as SQLink dot commands

[sqlite-utils](https://github.com/simonw/sqlite-utils) is Simon
Willison's Python CLI for manipulating SQLite databases. Its
~46 subcommands cover data ingest (CSV/JSON/TSV), schema
manipulation (transform, extract, add-foreign-key, etc), FTS5
helpers, and a handful of maintenance commands. The patterns
are battle-tested — porting them as SQLink dot commands gives
us a familiar surface that data folks already know how to
drive.

This plan groups the surface into ~5 dotcmd extensions and
phases the work by complexity. Stages 5e/5f/6 are done; the
architecture has spi-on-host plus the dotcmd-aware world, so
we can write each extension as a `.component.wasm` that
auto-embeds at startup. No further architectural work is
required.

## Goals

  - **Familiar surface.** Match sqlite-utils subcommand names
    1-to-1 where it makes sense (`.insert`, `.upsert`, `.transform`,
    `.search`, etc).
  - **Layered.** Five small extensions, each shippable on its
    own. Users (or downstream sqlink distributions) can opt
    out of any of them.
  - **Spi-native.** Every command runs against the host's
    shared spi connection — no per-extension db handle juggling.

## Non-goals

  - **Wire-compatible flag parsing.** sqlite-utils has dozens
    of flags per command (`--pk`, `--type`, `--csv`, `--tsv`,
    `--nl`, `--alter`, `--ignore`, `--replace`, `--no-headers`,
    `--detect-types`, ...). The first cut uses positional args
    + a small set of common flags; full parity is a follow-up.
  - **`install` / `uninstall` / `plugins`.** sqlite-utils has
    a pip-based plugin system. SQLink has `.sqlink` for
    components — we don't double up.
  - **Geospatial.** `add-geometry-column` and
    `create-spatial-index` need SpatiaLite. The SQLink survey
    already tracks a separate spatial extension; let that
    land independently and revisit then.
  - **`migrate`.** sqlite-utils-internal version table migration
    — not useful outside the Python lib.

## Architecture fit

Every command lives in a dotcmd extension implementing the
`dotcmd-aware` world. Inside `invoke(ctx)` we parse `ctx.args`
to dispatch on the subcommand, then either:

  - Run pure SQL via `spi.execute` / `execute_multi` /
    `execute_batch`. (~half the commands.)
  - Read a file (`std::fs::read_to_string`) + parse + emit
    `INSERT`s. (The ingest path.)
  - Read schema (`PRAGMA table_info`, `sqlite_master`) +
    rewrite (the transform / extract path).
  - Stream FTS5 builtins (`fts5(...)`, `MATCH`, etc).

No new spi methods are needed for the bulk of the work. A few
optional spi additions are flagged inline.

## Command inventory

Status column legend:
  - **Have** — already shipped (core-dotcmd or another extension).
  - **Port** — fits the dotcmd extension model cleanly.
  - **Skip** — out of scope (see Non-goals).
  - **Note** — implementation has a constraint worth flagging.

| sqlite-utils subcmd       | SQLink target                  | Status | Notes |
|---------------------------|--------------------------------|--------|-------|
| query                     | (eval loop)                    | Have   | Just type the SQL; output formats via `.mode` |
| memory                    | `.memory` (extension)          | Port   | Read JSON/CSV from stdin into an in-memory table |
| insert                    | `.insert` (utils-data)         | Port   | Phase 1: JSON/JSONL; Phase 2: CSV/TSV |
| upsert                    | `.upsert` (utils-data)         | Port   | Same parser as insert; uses `INSERT … ON CONFLICT` |
| bulk                      | `.bulk` (utils-data)           | Port   | One SQL template, many parameter sets |
| search                    | `.search` (utils-fts)          | Port   | Wraps `MATCH` against an FTS5 table |
| transform                 | `.transform` (utils-schema)    | Port   | Rename/retype/reorder cols via copy-and-swap |
| extract                   | `.extract` (utils-schema)      | Port   | Normalize denorm columns into a lookup table |
| schema                    | `.schema`                      | Have   | Already in core-dotcmd |
| insert-files              | `.insert_files` (utils-data)   | Port   | Reads file bytes into BLOB columns |
| analyze-tables            | `.analyze_tables` (utils-data) | Port   | DISTINCT count, NULL count, min/max per column |
| convert                   | `.convert` (utils-data)        | Note   | sqlite-utils accepts Python expr; we accept SQL via `UPDATE … SET col = <expr>` |
| tables                    | `.tables`                      | Have   | Core-dotcmd |
| views                     | `.views` (utils-schema)        | Port   | `SELECT name FROM sqlite_master WHERE type='view'` |
| rows                      | `.rows` (utils-data)           | Port   | `SELECT * FROM <table>` thin wrapper |
| triggers                  | `.triggers` (utils-schema)     | Port   | `SELECT name FROM sqlite_master WHERE type='trigger'` |
| indexes                   | `.indexes`                     | Have   | Already in core-dotcmd |
| create-database           | (cli flag)                     | Note   | The cli's `--db PATH` already creates if missing — wrap as `.create_database` for parity if desired |
| create-table              | `.create_table` (utils-schema) | Port   | Sugar over `CREATE TABLE` with `name:type` colspec |
| create-index              | `.create_index` (utils-schema) | Port   | Sugar over `CREATE INDEX` |
| create-view               | `.create_view` (utils-schema)  | Port   | `CREATE VIEW name AS <sql>` |
| migrate                   |                                | Skip   | sqlite-utils-internal versioning |
| enable-fts                | `.enable_fts` (utils-fts)      | Port   | Builds an `fts5` external content table |
| populate-fts              | `.populate_fts` (utils-fts)    | Port   | `INSERT INTO ... SELECT` from source table |
| rebuild-fts               | `.rebuild_fts` (utils-fts)     | Port   | `INSERT INTO fts(fts) VALUES('rebuild')` |
| disable-fts               | `.disable_fts` (utils-fts)     | Port   | DROP triggers + table |
| optimize                  | `.optimize` (utils-maint)      | Port   | `PRAGMA optimize` + FTS optimize loop |
| analyze                   | `.analyze` (utils-maint)       | Port   | `ANALYZE [tablename]` |
| vacuum                    | `.vacuum` (utils-maint)        | Port   | `VACUUM` — must run outside a tx |
| dump                      | `.dump`                        | Have   | cli builtin / serialize-cli |
| add-column                | `.add_column` (utils-schema)   | Port   | `ALTER TABLE ... ADD COLUMN` with type inference |
| add-foreign-key           | `.add_fk` (utils-schema)       | Port   | Requires the transform path |
| add-foreign-keys          | `.add_fks` (utils-schema)      | Port   | Plural-arg variant |
| index-foreign-keys        | `.index_fks` (utils-schema)    | Port   | Auto-index every FK col |
| enable-wal                | `.enable_wal` (utils-maint)    | Port   | `PRAGMA journal_mode=WAL` |
| disable-wal               | `.disable_wal` (utils-maint)   | Port   | `PRAGMA journal_mode=DELETE` |
| enable-counts             | `.enable_counts` (utils-maint) | Port   | Adds a `_counts` table + triggers |
| reset-counts              | `.reset_counts` (utils-maint)  | Port   | Recomputes `_counts` rows |
| duplicate                 | `.duplicate` (utils-schema)    | Port   | `CREATE TABLE new AS SELECT * FROM old` |
| rename-table              | `.rename_table` (utils-schema) | Port   | `ALTER TABLE ... RENAME TO` |
| drop-table                | `.drop_table` (utils-schema)   | Port   | `DROP TABLE` |
| drop-view                 | `.drop_view` (utils-schema)    | Port   | `DROP VIEW` |
| add-geometry-column       |                                | Skip   | Needs SpatiaLite |
| create-spatial-index      |                                | Skip   | Needs SpatiaLite |
| install / uninstall / plugins |                            | Skip   | Use `.sqlink` instead |

**Net new ports:** 32 commands across 4 extensions. ~14 are
schema, ~8 data, ~5 fts, ~5 maint.

## Crate layout

```
extensions/
  sqlite-utils-schema/   # 14 commands
  sqlite-utils-data/     # 8 commands
  sqlite-utils-fts/      # 5 commands
  sqlite-utils-maint/    # 5 commands
```

Each follows the `extensions/serialize-cli/` precedent: one
crate, `dotcmd-aware` world, a single `Manifest::describe()`
listing the dot commands with `id` per command, and an
`invoke()` that dispatches inside the extension.

## Stage breakdown

### Stage 1 — utils-schema (~1.5 days)

Lowest-risk start: every command is "parse args → emit SQL".

Phase 1.1 — read-only introspection (~half day)
  - `.views` — `SELECT name, sql FROM sqlite_master WHERE type='view'`
  - `.triggers` — same shape, type='trigger'

Phase 1.2 — simple writes (~half day)
  - `.create_table NAME COL:TYPE [COL:TYPE ...]` — translate
    `id:int,name:text,active:bool` to a `CREATE TABLE` stmt.
    Optional `--pk COL` flag.
  - `.create_index TABLE COL [COL …]` — `CREATE INDEX idx_T_C`
  - `.create_view NAME SQL`
  - `.drop_table NAME [--ignore]`
  - `.drop_view NAME [--ignore]`
  - `.rename_table OLD NEW`
  - `.duplicate OLD NEW` — `CREATE TABLE NEW AS SELECT * FROM OLD`
  - `.add_column TABLE COL TYPE`

Phase 1.3 — transform + extract (~half day)
  - `.transform TABLE` — supports `--rename`, `--drop`,
    `--type COL TYPE`, `--column-order`, `--pk`. Implemented
    as: create new table with desired schema, `INSERT ... SELECT`,
    drop old, rename new. (sqlite-utils's `Table.transform()`
    in Python.)
  - `.extract TABLE COL [COL …]` — pick out denormalized
    columns, create a lookup table keyed on (id, …), replace
    the cols in the source with an FK.
  - `.add_fk TABLE COL OTHER_TABLE [OTHER_COL]` — uses the
    transform path (sqlite doesn't allow `ALTER TABLE ... ADD
    CONSTRAINT` for FKs).
  - `.add_fks` — vector variant.
  - `.index_fks` — walk `PRAGMA foreign_key_list` for every
    table, create an index for each FK column.

Smokes: round-trip a small db through every command. Verify
schemas via `PRAGMA table_info` and FK lists via
`PRAGMA foreign_key_list`.

### Stage 2 — utils-data ✓ shipped (commit `9e01a19`)

All 8 commands ported as `extensions/sqlite-utils-data/` (334 KB
component). Schema inference uses a `Null<Integer<Real<Text`
widening lattice. JSON, JSONL, CSV, TSV all supported via format
flag or extension sniff. CSV/TSV cells coerce to numeric per
inferred column type on the second pass. `.memory` attaches
`:memory:` as `mem` schema and delegates to `.insert mem.<basename>`
via schema-qualified identifier paths. Smoke walks the full
fixture (insert → tables → rows → upsert → analyze_tables →
convert → insert_files).

Plan retained below.

### Stage 2 — utils-data (~2 days)

Phase 2.1 — `.rows` + `.analyze_tables` (~half day)
  - `.rows TABLE [N]` — `SELECT * FROM TABLE LIMIT N`
  - `.analyze_tables [TABLE [TABLE …]]` — for each text/int
    column: `COUNT(DISTINCT col)`, `COUNT(*) - COUNT(col)`
    (null count), `MIN`, `MAX`, top-N most common via
    `GROUP BY col ORDER BY count(*) DESC LIMIT 10`.

Phase 2.2 — `.insert` + `.upsert` JSON path (~1 day)
  - `.insert TABLE FILE` reads FILE as JSON array of objects
    or JSONL. Schema inference: walk first N rows, pick the
    widest type per column. Create table if missing. Batch
    `INSERT INTO TABLE(...) VALUES(?, ?, ...)` per row (use
    `spi.execute` per row for now; later move to prepared +
    bind-many once `spi.prepare`/`bind`/`step` are exposed).
  - `.upsert TABLE FILE --pk COL` — same plus `ON CONFLICT(pk)
    DO UPDATE SET col=excluded.col`.

Phase 2.3 — CSV + `.bulk` + `.insert_files` (~half day)
  - `.insert TABLE FILE --csv` — pure-Rust `csv` crate
    parser. Type inference: try int, then real, then text.
  - `.bulk TABLE SQL FILE` — read JSONL, bind each row as
    params to a prepared template. (Needs `spi.prepare` /
    `bind` / `step` so we don't re-parse the SQL per row;
    alternatively `spi.execute` repeatedly — slower but works
    on existing surface.)
  - `.insert_files TABLE FILE [FILE …]` — read each file's
    bytes into a BLOB column named `content` plus `path`,
    `name`, `size` columns.

Phase 2.4 — `.convert` + `.memory` (~half day)
  - `.convert TABLE COL SQL_EXPR` — `UPDATE TABLE SET COL =
    (SQL_EXPR)`. sqlite-utils accepts Python expressions; we
    accept SQL because users already have the full SQLite
    function catalogue (sha3_256, uuid, etc) through scalars
    on the spi conn.
  - `.memory FILE [FILE …]` — load each file as a temp table
    in `:memory:`, then run subsequent commands against it.
    Implementation: `ATTACH ':memory:' AS mem`, run
    `.insert mem.t1 FILE` per file.

### Stage 3 — utils-fts (~1 day) — SHIPPED

`extensions/sqlite-utils-fts/` ports all 5 commands as a single
dot-command extension auto-embedded by the cli alongside
core-dotcmd / session-cli / etc. Pure SQL on the host's shared
spi connection — no new spi imports required.

  - `.enable_fts TABLE COL [COL …] [--create-triggers]
     [--tokenize T]` — creates `<T>_fts` external-content FTS5
    virtual table indexing the named columns of TABLE.
    Optional AFTER INSERT/DELETE/UPDATE triggers keep the index
    in sync; populates immediately. Custom tokenizer via
    `--tokenize porter` etc.
  - `.disable_fts TABLE` — drops `<T>_fts` and the
    `<T>_ai`/`<T>_ad`/`<T>_au` triggers (the convention from
    sqlite-utils).
  - `.rebuild_fts [TABLE]` —
    `INSERT INTO <T>_fts(<T>_fts) VALUES('rebuild')`. Without an
    arg, rebuilds every `*_fts` table found in `sqlite_master`.
  - `.populate_fts TABLE COL [COL …]` —
    `INSERT INTO <T>_fts(rowid, COL…) SELECT rowid, COL… FROM
    <T>`. Mostly redundant with `.rebuild_fts` for
    external-content tables; ships for sqlite-utils parity.
  - `.search TABLE QUERY [--limit N] [--columns col1,col2,…]` —
    `SELECT … FROM <T> WHERE rowid IN (SELECT rowid FROM <T>_fts
    WHERE <T>_fts MATCH ?1 ORDER BY rank LIMIT ?2)`. Default
    LIMIT 20. Query is bound as a param; the user can use
    FTS5's phrase / prefix / column-filter syntax directly
    (e.g. `.search articles sql*` for prefix, `.search articles
    title:hello` for column filter).

Component size: 143 KB. Smoke (CREATE TABLE + INSERT +
.enable_fts --create-triggers + .search + INSERT-triggers-fire
+ .rebuild_fts + .disable_fts cleanup) all green.

### Stage 4 — utils-maint (~half day) — SHIPPED

`extensions/sqlite-utils-maint/` ports all 8 commands. Smoked
end-to-end: trigger-maintained `_counts` table tracks inserts/
deletes correctly; `.reset_counts` recomputes from scratch;
`.analyze` / `.optimize` / `.vacuum` / `.enable_wal` /
`.disable_wal` / `.create_database` all clean. The cli got one
companion change: `db/path` is now pushed into the cli-state
snapshot so `.create_database` can report the active database
path (was missing from build_cli_state_snapshot).

  - `.vacuum` — `VACUUM` (must end any open tx).
  - `.analyze [TABLE]` — `ANALYZE` (whole db or one table).
  - `.optimize` — `PRAGMA optimize` + FTS optimize per
    `_fts` table found.
  - `.enable_wal` — `PRAGMA journal_mode=WAL`.
  - `.disable_wal` — `PRAGMA journal_mode=DELETE`.
  - `.enable_counts` — create `_counts(table_name, count)`,
    seed with `SELECT count(*) FROM each`, install
    AFTER INSERT/DELETE triggers to maintain.
  - `.reset_counts` — recompute every row.
  - `.create_database` — no-op when the cli was started with
    `--db PATH`; print the path. Exists for sqlite-utils
    parity.

### Stage 5 — polish + parity (~1 day)

  - `.help <cmd>` long-form text matching sqlite-utils `--help`
    where it provides usable extra context (especially for
    `transform` and `extract`).
  - Top-N flag coverage: `--csv`/`--tsv`/`--nl` on ingest,
    `--limit`/`--offset` on query-like commands.
  - Smoke regression: a small fixture db (analog to
    sqlite-utils's `dogs.db` examples) walked through every
    command.

## Dependencies

  - **`csv` crate** for CSV/TSV parsing. Wasm-friendly, no
    deps.
  - **`serde_json`** is already in the workspace; reuse.
  - **No new spi methods required for stages 1-4.** Stage 2.3
    (`.bulk`) benefits from `spi.prepare`/`bind`/`step`
    exposure for prepared-stmt reuse — those already exist
    in the `prepared` interface. If the existing surface is
    enough for a per-row `spi.execute` loop, ship that first
    and add a fast path later.
  - Optional: `spi.execute-many(sql: string, rows: list<list<sql-value>>) -> result<...>`
    would let the data extension push 10k-row batches in one
    crossing instead of N+1 wasm calls. Worth ~5-10x on
    ingest. Defer until benchmarks show a need.

## Open questions

  1. **Naming convention.** sqlite-utils uses `insert-files`
     (kebab); SQLink dot commands use underscores
     (`.insert_files`). Pick underscores for consistency with
     existing core-dotcmd surface; document the mapping in
     each extension's help text.
  2. **Output format.** sqlite-utils defaults to JSON, has
     `--csv`/`--tsv`/`--nl`/`--table`/`--fmt`. SQLink dot
     commands return strings the cli renders inline. Use
     the cli's existing `.mode` for output formatting; don't
     duplicate the flag matrix in each command. Override only
     where sqlite-utils's default differs in a useful way
     (e.g. `.rows` defaults to `.mode column`).
  3. **Whether to ship as one big extension or four.** Four
     keeps each component small (~50-100 KB compiled) and
     lets distributions opt out (e.g. embedded sqlink without
     the data-ingest paths). One extension is simpler. Plan
     assumes four.
  4. **`.memory` semantics.** sqlite-utils-`memory` runs a
     query against in-memory data without touching disk.
     SQLink's `--db` arg always opens a file. Either: open an
     in-mem db for the duration of `.memory`'s subcommand
     (requires `spi.open-db ":memory:"` round-trip); or attach
     `:memory:` as a named schema (`ATTACH ':memory:' AS mem`)
     so the user's main db is preserved. Latter is simpler;
     pick it unless `:memory:`-only semantics turn out to
     matter.

## Out of scope

  - Geospatial commands (need SpatiaLite — separate plan).
  - The pip-style plugin system (we have `.sqlink`).
  - sqlite-utils's Python API — this plan is CLI-only.
  - Performance parity on huge-ingest workloads (10M rows).
    The per-row `spi.execute` path is 100-1000x faster than
    nothing, but sqlite-utils's prepared+chunked path is
    faster. Track separately if it shows up.

## Effort estimate

| Stage              | Effort   | Cumulative |
|--------------------|----------|------------|
| 1 utils-schema     | 1.5 days | 1.5        |
| 2 utils-data       | 2 days   | 3.5        |
| 3 utils-fts        | 1 day    | 4.5        |
| 4 utils-maint      | 0.5 day  | 5          |
| 5 polish + parity  | 1 day    | 6          |

Total: ~6 working days for the 32-command port. Could
parallelize stages 1-4 if multiple agents work in parallel
on separate extensions, since they share no code.

## Recommended commit order

Mirror PLAN-cli-stages-5-6.md style:

  1. Scaffolding commit per extension (Cargo.toml + empty
     manifest + dispatcher skeleton).
  2. One commit per dot command (small, easy to bisect).
  3. Per-stage doc update in this plan once shipped.

## Smoke checklist

After every stage, this set should pass against a fresh
`/tmp/sqlink-utils-test.db`:

```
.create_table dogs id:int name:text age:int --pk id
.insert dogs /tmp/dogs.json
.tables
.rows dogs
.add_column dogs breed text
.transform dogs --type age real --rename age age_years
.enable_fts dogs name
.search dogs woof
.analyze_tables dogs
.duplicate dogs dogs_backup
.drop_table dogs --ignore
.vacuum
```
