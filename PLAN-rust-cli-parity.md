# Plan: Rust CLI parity with sqlite3

Goal: bring `cli/` (the Rust SQLite CLI that targets the
`sqlite-cli-command` world) to feature parity with stock
`sqlite3`'s dot-command surface, for every command that makes
sense in a wasm context. CLI compatibility is the contract a
SQLite user expects; we should honor it.

This plan is the Rust-CLI sibling of `PLAN-cli-commands.md`
(which scopes the same work for the C CLI binary built from
`src/cli/sqlite_cli.c`). They land independently. Implementations
diverge — Rust uses `core::db` and the in-tree `cli/src/dot.rs`
dispatcher; C uses the sqlite3 C API and `do_meta_command()`.

## Scope reference

`sqlite3 --help` lists ~60 dot-commands. This plan bins them.

### Already in `cli/` (24)

In `cli/src/dot.rs` dispatcher:

`.help`, `.show`, `.tables`, `.schema`, `.indexes`, `.databases`,
`.headers`, `.mode`, `.nullvalue`, `.separator`, `.echo`,
`.prompt`, `.print`, `.bail`

In `cli/src/lib.rs::eval_input` (project-custom + base mechanics):

`.quit`/`.exit`, `.load`, `.unload`, `.open`, `.fiji` (to be
renamed to `.run`, see below), `.register-resolver`,
`.unregister-resolver`, `.resolvers`, `.register-provider`,
`.cache`

### Wasm-incompatible — skip

| Command | Reason |
|---|---|
| `.shell CMD` | requires OS shell |
| `.system CMD` | same |
| `.cd DIRECTORY` | wasm sandbox |
| `.excel` | requires external app |
| `.crlf on\|off` | Windows-specific line endings |
| `.connection ?NUM?` | multi-connection state we don't model |
| `.expert` | requires expert extension |
| `.filectrl CMD ...` | low-level file control |
| `.imposter INDEX TABLE` | internal advanced feature |
| `.intck` | requires intck extension |
| `.nonce STRING` | safe-mode bypass |
| `.progress N` | low-level progress callback |
| `.recover` | corruption recovery |
| `.scanstats on\|off\|est` | requires `SQLITE_ENABLE_STMT_SCANSTATUS` |
| `.session ?NAME? CMD ...` | requires session extension |
| `.testcase NAME` / `.testctrl` | internal testing only |

## Phased rollout

Each phase is one commit (or a small batch if a single command is
substantial, like `.import`'s CSV parsing).

### Phase 1 — minimum-viable parity (7 commands)

What a sqlite3 user reaches for inside the first session.

| Command | Notes |
|---|---|
| `.read FILE` | Open FILE, read line-by-line, accumulate buffer, fire on `sqlite3_complete`, dispatch dot-commands recursively. Echo when `.echo on`. Errors stop unless `.bail off`. |
| `.output ?FILE?` | Switch the cli's `Write` target to FILE; bare `.output` resets to stdout. Add `output: Box<dyn Write>` field to `Settings`. |
| `.once ?FILE?` | Same as `.output` but resets to stdout after the next command's output is flushed. |
| `.version` | Print sqlite library version (`db::version()`), the crate's `CARGO_PKG_VERSION`, the wasm target triple. |
| `.width N N ...` | Update `Settings.column_widths: Vec<usize>`. Read by the `column` and `box` output modes. |
| `.changes on\|off` | When on, print `Changes: N, Total: M` after each successful statement. Use `conn.changes()` + `conn.total_changes()`. |
| `.timer on\|off` | When on, print wall-clock duration after each statement. `std::time::Instant` before/after the `stmt.step()` loop. |

Implementation surface: `cli/src/dot.rs`, `cli/src/settings.rs`,
`cli/src/format.rs`. New module `cli/src/script.rs` for `.read`'s
recursive evaluator to keep `lib.rs::eval_input` from growing.

### Phase 2 — data management (6 commands)

The "I want to move data between files" surface.

| Command | Notes |
|---|---|
| `.import FILE TABLE` | Parse CSV (or whatever the current `.mode` is). Build prepared INSERT. Bind each row. Handle column-name auto-detection from first line per sqlite3 convention. |
| `.dump ?TABLE?` | Walk schema (sqlite_master), emit `CREATE` + `INSERT` statements. With TABLE pattern, filter. Output is valid SQL replayable via `.read`. |
| `.backup ?DB? FILE` | Use `sqlite3_backup_init` / `_step` / `_finish` directly (core::db doesn't wrap these yet — add a method, or expose raw handle). |
| `.restore ?DB? FILE` | Reverse: open FILE, backup INTO main db. Same API. |
| `.save FILE` | Alias for `.backup main FILE`. |
| `.clone NEWDB` | Same as backup but to a NEW file path. Refuse if NEWDB exists. |

### Phase 3 — query analysis (6 commands)

Debug + dev workflow.

| Command | Notes |
|---|---|
| `.timeout MS` | `core::db::Connection::busy_timeout(ms)` — needs adding to `core::db` (sqlite3_busy_timeout). |
| `.trace ?FILE\|on\|off?` | sqlite3_trace_v2 with SQLITE_TRACE_STMT. Add `Connection::set_trace(callback)` to core. Output to current `.output` or to FILE. |
| `.explain ?on\|off\|auto?` | When on, wrap subsequent SQL with `EXPLAIN`. `auto` enables for queries starting with `EXPLAIN` keyword. State in Settings. |
| `.eqp ?on\|off?` | Same as `.explain` but with `EXPLAIN QUERY PLAN`. |
| `.stats ?on\|off?` | Print row count + memory + sqlite3 status counters after each statement. |
| `.parameter init/list/set NAME VALUE/clear/unset NAME` | Track a `HashMap<String, db::Value>` in Settings. The eval path looks for `:name` / `$name` / `@name` parameters in prepared statements and binds from this map. |

### Phase 4 — db introspection (7 commands)

| Command | Notes |
|---|---|
| `.fullschema ?--indent?` | `SELECT sql FROM sqlite_master WHERE sql IS NOT NULL` + `SELECT sql FROM sqlite_stat1`. Optionally pretty-print via simple SQL formatter. |
| `.dbinfo ?DB?` | Walk `pragma_database_list`, `pragma_page_count`, `pragma_page_size`, `pragma_freelist_count`, `pragma_encoding`, `pragma_journal_mode`. Pretty-print as a table. |
| `.dbconfig ?op? ?val?` | Map `op` string to `SQLITE_DBCONFIG_*` constants; pass to `sqlite3_db_config`. Add to core. |
| `.limit ?LIMIT? ?VAL?` | Bare prints all `sqlite3_limit(SQLITE_LIMIT_*, -1)` values; with LIMIT/VAL sets one. Add to core. |
| `.log on\|off\|FILE` | `sqlite3_config(SQLITE_CONFIG_LOG, ...)` — process-global, not connection-scoped. Track `enabled: bool` in Settings; write to current `.output` or to FILE. |
| `.binary on\|off` | When on, write blob columns raw to current `.output` instead of hex-encoded. State in Settings; read by `format.rs`. |
| `.auth on\|off` | When on, install an authorizer callback that traces every action and prints to stderr. Mostly a debugging aid. |

### Phase 5 — niche (5 commands)

| Command | Notes |
|---|---|
| `.lint OPTIONS` | `OPTIONS = fkey-indexes` reports foreign keys without backing indexes. Single SQL query against `pragma_foreign_key_list` + `pragma_index_list` joins. |
| `.archive --create FILE FILES...` | Requires zip support; depends on `wit/zip-operations.wit`. Defer unless the zip path comes online. |
| `.sha3sum ?--schema? ?TABLE?` | Compute SHA3 over the canonical encoding of database contents. Requires SHA3 — link to a small Rust SHA3 crate (already in workspace via blake3? — no, sha3 is separate). |
| `.vfslist` | Walk `sqlite3_vfs_find(NULL)`'s linked list. Single VFS in our build by default — output is short. |
| `.vfsname ?AUX?` | Print the current connection's VFS name via `sqlite3_file_control(SQLITE_FCNTL_VFSNAME)`. |

## Custom commands (preserve)

Not in sqlite3; this project's additions. Stay.

`.run` (renamed from `.fiji`), `.unload`, `.register-resolver`,
`.unregister-resolver`, `.resolvers`, `.register-provider`,
`.cache`.

## The `.fiji` → `.run` rename

Decided in PR review on this plan's preceding conversation
(2026-06-13). Rationale: the Fiji name collides with the
out-of-tree `~/git/fijivm` project (a JVM-to-WebAssembly port).
`.run` is the natural verb — it matches the existing `fiji.run()`
method name on the WIT interface, and it pairs cleanly with
`.read FILE` (SQL files) once Phase 1 ships.

Touches:

- `wit/fiji.wit` → `wit/run.wit`. Interface `fiji { run: func() ... }`
  becomes `run { invoke: func() ... }` (or stays `run.run` for the
  one-letter difference; pick at implementation time).
- `wit/fiji.wit`'s `world fiji-function` → `world runnable` (the
  shape becomes its concept name).
- `host/src/lib.rs`: `pub mod fiji {}` → `pub mod run {}`,
  `FijiState` → `RunState`, `FijiHostWrap` → `RunHostWrap`,
  `FijiHostData` → `RunHostData`, `make_fiji_linker` →
  `make_run_linker`.
- `wit/extension-loader.wit`: `run-fiji-function` → `run-wasm`
  (or `dispatch-run`).
- `host/tests/load.rs`: rename `fiji_*` tests to `run_*`.
- `cli/src/lib.rs`: `.fiji` dispatch + `do_fiji()` → `.run` /
  `do_run()`.
- `AUTHORING-FIJI-FUNCTIONS.md` → `AUTHORING-RUN-COMPONENTS.md`,
  body sweep.
- `sqlite-wasm-loader/runtimes/wasmtime/fiji-hello/` etc. —
  out-of-tree submodule; rename there in a separate PR.

This rename can land **alongside** Phase 1, or as its own commit
before Phase 1. Order doesn't matter — they touch different files.

## Multi-language `.run hello.py` (follow-on, not part of this plan)

After `.run` exists for wasm files, the next layer is dispatching
non-wasm files to a language runtime by file extension. Concrete
shape:

- `.run hello.py` → look up the registered Python runtime
  (e.g. `python-wasm` / `python.composed.wasm` from the
  `tegmentum-webassembly-sdk`), instantiate it, pass the `.py`
  source via stdin or a host import, invoke `run()`.
- `.run hello.java` → look up the registered JVM runtime
  (`fijivm`), same path.
- Same for `.r` (jollyroger), `.go` (compiled .wasm via the
  Go toolchain or invocation of `tg run`), etc.

The Tegmentum SDK's `tg run` already does this dispatch
externally. Two ways to integrate:

1. **Shell out to `tg run`** — easy, but requires `tg` on PATH
   and uses an external process. Loses the wasm sandbox guarantee.
2. **Register language runtimes as compose providers** —
   `.register-provider python python-wasm.component.wasm` (or
   automatic discovery via a manifest). `.run hello.py` then
   resolves the `python` provider and invokes it. Pure in-process,
   keeps the sandbox.

Option 2 is the architecturally honest path and reuses the existing
`compose:dynlink/linker` plumbing. Real plan when we get there;
out of scope for this document.

## Test plan

For each implemented command, a bash integration test under
`tests/cli/` that pipes input to `./sqlite-wasm` (the shell wrapper)
and asserts on stdout. The test cases should be runnable against
stock `sqlite3` for comparison — if both produce the same output,
parity is real.

Example (`tests/cli/dot-read.sh`):

```bash
#!/usr/bin/env bash
set -euo pipefail
DB=$(mktemp).db
SCRIPT=$(mktemp).sql
cat > "$SCRIPT" <<'SQL'
CREATE TABLE t(x);
INSERT INTO t VALUES (1),(2),(3);
SELECT count(*) FROM t;
SQL

EXPECTED="3"
GOT_WASM=$(./sqlite-wasm "$DB" ".read $SCRIPT" 2>&1 | tail -1)
GOT_SQLITE3=$(sqlite3 "$DB" ".read $SCRIPT" 2>&1 | tail -1)
[[ "$GOT_WASM" == "$EXPECTED" ]] || { echo "wasm:  got '$GOT_WASM'"; exit 1; }
[[ "$GOT_SQLITE3" == "$EXPECTED" ]] || { echo "sqlite3: got '$GOT_SQLITE3'"; exit 1; }
echo OK
```

A Rust integration test under `host/tests/` invokes the bash
scripts so `cargo test` runs them.

## Estimated commits

| Phase | Commands | Rough commits |
|---|---|---|
| Rename `.fiji` → `.run` | n/a | 1 |
| Phase 1 — basic parity | 7 | 1–2 |
| Phase 2 — data management | 6 | 2 (`.import` is heavy; everything else is light) |
| Phase 3 — query analysis | 6 | 2 (`.parameter` is its own commit) |
| Phase 4 — db introspection | 7 | 1–2 |
| Phase 5 — niche | 5 | 1 (skip `.archive` for now) |
| **Total** | **31** | **8–10** |

## Suggested order

1. Rename `.fiji` → `.run` (independent, mechanical, paves the way).
2. Phase 1 — gets us to "users won't immediately notice this isn't sqlite3."
3. Phase 2 — the next thing people reach for.
4. Phases 3 / 4 / 5 — opportunistic; finish when needed.

After Phase 1 lands, revisit whether to start the multi-language
`.run hello.py` work in parallel, or finish CLI parity first.
