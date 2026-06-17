# cli dot-command cheatsheet

Every dot-command the cli implements, with one-line purpose
and a "smoke?" column flagging when it's useful inside an
extension's `smoke.sql` (or pre-pended to `smoke.sql` by
`tooling/smoke.py`).

The harness automatically prepends `.nullvalue <NULL>` to every
smoke run (T-19); your `smoke.sql` doesn't write it.

Authoritative source: `cli/src/dot.rs` dispatch (~line 41).
If this drifts from the dispatch list, re-run T-21's audit.

## Quick reference

| Command          | Smoke? | One-line purpose                                          |
|------------------|--------|-----------------------------------------------------------|
| `.help`          |        | List built-in dot-commands                                |
| `.show`          |        | Dump current settings (mode, headers, prompt, ...)        |
| `.version`       |        | Print bundled SQLite version                              |
|                  |        |                                                            |
| `.tables [PAT]`  |        | List tables (optional glob filter)                        |
| `.schema [TBL]`  |        | Print CREATE statements (optional table filter)           |
| `.indexes [TBL]` |        | List indexes (optional table filter)                      |
| `.databases`     |        | List attached databases (main, temp, ...)                 |
| `.dbinfo`        |        | Pragma-derived db stats (page size, count, ...)           |
| `.dbconfig`      |        | Dump SQLITE_DBCONFIG_* runtime flags                      |
| `.fullschema`    |        | `.schema` + all triggers + `sqlite_stat1` content         |
|                  |        |                                                            |
| `.headers on\|off`|       | Toggle column-name headers in output                      |
| `.mode <m>`      |        | Output mode: `list` (default), `column`, `csv`, `tabs`    |
| `.nullvalue <s>` |  auto  | Render NULL as `<s>` instead of empty (T-19 auto-injects) |
| `.separator <s>` |        | Set column / row separator (depends on mode)              |
| `.width N N N`   |        | Column widths in `column` mode                            |
| `.binary on\|off`|        | Toggle BLOB hex dump on output                            |
|                  |        |                                                            |
| `.echo on\|off`  | .read  | Echo each statement before running. Only fires inside     |
|                  |  only  | `.read FILE`; stdin-piped smoke input does NOT echo.      |
| `.print STR`     |  yes   | Emit STR verbatim. Use for section headers in smoke.sql.  |
| `.changes on\|off`|       | Show "Changes: N" after each INSERT/UPDATE/DELETE         |
| `.timer on\|off` |        | Show wall + CPU time after each statement                 |
| `.timeout MS`    |        | Busy-handler timeout (ms)                                 |
| `.bail on\|off`  | .read  | Stop on first error. Only fires inside `.read FILE`;      |
|                  |  only  | smoke (stdin-piped) keeps running past errors regardless. |
|                  |        |                                                            |
| `.explain on\|off`|       | Show EXPLAIN output for following queries                 |
| `.eqp on\|off`   |        | Show EXPLAIN QUERY PLAN before each query                 |
| `.stats on\|off` |        | Print sqlite3 stmt-status counters after each query       |
| `.parameter ...` |        | Manage bound parameters (`set`, `unset`, `list`, `init`)  |
| `.limit ...`     |        | Get/set SQLITE_LIMIT_* values                             |
| `.prompt P [C]`  |        | Set primary [+ continuation] prompt strings               |
|                  |        |                                                            |
| `.lint fkey-indexes` |    | Report FKs with no covering index                         |
| `.sha3sum [TBL]` |        | Stable SHA3 hash of one/all tables                        |
| `.vfslist`       |        | List registered VFSes (memdb, wasivfs, ...)               |
| `.vfsname`       |        | Print active VFS for current connection                   |
| `.archive ...`   |        | Create/extract sqlar zipfile-style archives               |
| `.log on\|off`   |        | Toggle sqlite3_log callback echo (handled in `lib.rs`)    |

## Smoke-test idioms

- **NULL handling**: just write `<NULL>` in `smoke.expected`.
  The harness pre-injects `.nullvalue <NULL>`. Don't call it
  in your `smoke.sql`  redundant, and it would override.

- **Section headers in smoke.sql**: when one smoke file has many
  blocks (e.g. per-mode tests for a vtab), use `.print` to emit
  visible separators and assert them with the same string in
  `smoke.expected`. Cheaper than reading raw rows in context.

- **`.echo` / `.bail` don't help in smoke.** They only fire
  for statements run via `.read FILE`. The harness pipes
  smoke.sql through stdin, not `.read`, so these settings are
  ignored. If you need first-failure-wins, structure the
  smoke file so cascading failures produce distinct rows
  (or use a `.print '--- section ---'` marker so a diff
  pinpoints the failing block).

## Things the cli does that surprise smoke authors

- `round(real, n)` results that land on integers print as
  `21`, not `21.00`. Don't pad `smoke.expected` with phantom
  decimal precision  match what the cli actually prints.
- `--` line comments are stripped by `tooling/smoke.py` before
  the SQL reaches the cli, because the cli's parser fuses a
  leading `--` with the following dot-command (T-9). Use `/*
  ... */` block comments inside `smoke.sql` if you want
  inline documentation.
- A trailing blank line is normal; `parse_results()` strips
  blanks. Don't end `smoke.expected` with one and expect a
  match  it'll just be ignored.
