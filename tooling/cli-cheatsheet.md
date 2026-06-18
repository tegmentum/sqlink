# cli dot-command cheatsheet

Every dot-command the cli implements, with one-line purpose
and a "smoke?" column flagging when it's useful inside an
extension's `smoke.sql` (or pre-pended to `smoke.sql` by
`tooling/smoke.py`).

The harness automatically prepends `.nullvalue <NULL>` to every
smoke run (T-19); your `smoke.sql` doesn't write it.

Authoritative source: `cli/src/dot.rs` dispatch (~line 41)
AND `cli/src/lib.rs` eval_input (handles ~20 commands inline
via `strip_prefix` before the dispatcher). If this drifts
from either, re-run T-21's audit.

## Parity with upstream sqlite3 cli

We implement ~57 dot commands; upstream sqlite3 cli has ~70.
Gaps are either wasi-sandbox blocked (`.shell`, `.system`,
`.cd`) or genuinely niche/experimental
(`.testcase`/`.check`/`.scanstats`/`.expert`/`.imposter`/
`.filectrl`/`.nonce`/`.crnl`/`.vfsinfo`/`.intck`/`.progress`/
`.recover`/`.connection`/`.dbtotxt`/`.excel`). Not pretending
to match those.

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
| `.session NAME ...` |     | Session/changeset (matches upstream sqlite3 cli)          |
| `.session list`  |        | List active sessions                                       |
|                  |        |                                                            |
| `.dump [PAT]`    |        | Render schema + data as replayable SQL                    |
| `.import F TBL`  |        | Load FILE (CSV/TSV per `.mode`) into TABLE                |
| `.backup ?DB? F` |        | Copy live db to FILE (alias `.save F` / `.clone F`)       |
| `.restore ?DB? F` |       | Read FILE back into DB (default `main`)                   |
| `.open ?FILE?`   |        | Switch to a different db file                             |
| `.output FILE`   |        | Redirect SQL output to FILE until `.output stdout`        |
| `.once FILE`     |        | Redirect output for the next command only                 |
| `.trace ?OPT?`   |        | Log SQL statements as they execute                        |
| `.auth on\|off`  |        | Toggle authorizer dispatch                                |
| `.quit` / `.exit`|        | Terminate the cli                                          |
|                  |        |                                                            |
| `.load FILE`     |        | Load an extension component (this project's `.load`)      |
| `.unload NAME`   |        | Unload an extension                                        |
| `.cache ?op?`    |        | Inspect/manage component cache (project-specific)         |
| `.compose ...`   |        | sqlite-wasm-loader compose ops (project-specific)         |
| `.grants ...`    |        | Capability grants for loaded extensions (project-specific)|
| `.resolvers`     |        | List loaded resolvers (project-specific)                  |
| `.runtimes`      |        | List runtime providers (project-specific)                 |
| `.run ?args?`    |        | Run a sqlite-runtime composed wasm component              |
|                  |        |                                                            |
| `.serialize F`   |        | Write live `main` db to FILE as a blob (sqlite3_serialize) |
| `.deserialize F` |        | Load FILE into `main` as an in-memory db (sqlite3_deserialize) |

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

## Session/changeset commands (sqlite3 cli compat)

`.session` mirrors the upstream sqlite3 shell's session API. Each
named session attaches to the cli's connection and tracks changes
until you extract the changeset:

```
.session main create               # create session "main"
.session main attach *             # attach all tables (or NAME)
CREATE TABLE t(x);                 # ... changes get captured
INSERT INTO t VALUES (1), (2);
.session main isempty              # 0 (changes exist) / 1 (no changes)
.session main changeset out.cs     # write captured changeset to FILE
.session main patchset out.cs      # smaller variant (no before-values)
.session main enable on|off        # toggle change recording
.session main indirect on|off      # toggle indirect-changes flag
.session main delete               # close + free
.session list                      # list active session names
```

Path arg to `changeset` / `patchset` is relative to wasi cwd
preopens  use a path inside the directory you invoked
sqlite-wasm-run from. Absolute paths outside wasi preopens fail
with "No such file or directory" (this is wasi sandboxing, not a
session bug).

The captured blob is byte-compatible with `sqlite-wasm-run
changeset {invert|concat|apply}` host subcommands  capture
inside the cli, transform / replicate outside.

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

## Harness output limitations

These are NOT cli quirks  they're side effects of how
`tooling/smoke.py` parses the cli's stdout. Mostly relevant
when an extension's output overlaps with the parser's strip
rules.

- **Leading whitespace is eaten by the prompt regex.** Output
  like `"  hi"` (two spaces + text) becomes `"hi"` after
  parsing, because `PROMPT_RE = ^(sqlite>\s*|...)+` greedily
  consumes the trailing whitespace of the `sqlite>` prompt
  along with any user-output spaces immediately following.
  Workaround: in smoke tests for `pad_left`-style scalars,
  use a non-whitespace fill character (`.`, `0`, `_`) so the
  pad is visible after parsing. Surfaced via `numfmt`'s
  `numfmt_pad_left` smoke.
- **Integer-valued reals lose `.00`.** `round(3.0, 2)` prints
  as `3`, not `3.00`. Match what the cli prints, not what you
  expect mathematically. Surfaced via `color`'s WCAG contrast
  smoke (contrast=21 not 21.00).
- **Hex output collides with the comment marker.** `smoke.expected`
  treats `# foo` as a comment but a literal `#ff8800`
  (color hex) is NOT a comment  parse_expected requires `#`
  + whitespace. Surfaced via `color`'s hex output smoke.
- **NULLs render as the literal string `<NULL>`** because the
  harness pre-injects `.nullvalue <NULL>` (T-19). Write the
  sentinel verbatim in `smoke.expected`.
- **Empty-string outputs are dropped.** `parse_results` skips
  blank lines (to filter the load banner + trailing prompt),
  so a scalar that returns `""` is indistinguishable from
  "no row" in the parsed output. Workaround: in smoke,
  wrap with a sentinel:
  `SELECT coalesce(nullif(f(), ''), '<empty>');`
  This is NOT solvable by another `.nullvalue`-style cli
  directive  empty string and NULL are different types,
  and the parser strips blanks for the prompt-noise reason.
  Most cleanly surfaced by any function that returns an
  empty string by spec  catch via
    `SELECT coalesce(nullif(f(), ''), '<empty>');`
