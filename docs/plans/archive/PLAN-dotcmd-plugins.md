# PLAN: pluggable dot commands

## Goal

Turn the SQLite CLI surface into a registry of dot commands that's
**one declaration backed by three equivalent entry points**:

| entry point | shape | example |
|---|---|---|
| Interactive shell | `.NAME ARGS...` | `.import-json users users.json` |
| CLI argv | `sqlink [db] .NAME ARGS...` or `sqlink NAME ARGS...` (git-style) | `sqlink my.db .import-json users users.json` |
| SQL programmatic | `dot_command(NAME, ARG, ...)` | `SELECT dot_command('import-json', 'users', 'users.json');` |

Every command has ONE registration (the `dot-command-spec` in the
manifest) and ONE implementation (`dot-command.invoke`). The cli's
job is to feed three different front-ends into the same dispatcher.

The registry itself is sourced from two layers:

1. **Session layer** — extensions loaded into the running cli via
   `.load FILE.wasm` (or `--load`). Stateless between sessions.
   Built-ins are nothing more than a SESSION-layer extension that
   ships preloaded with the distribution.

2. **Database layer** — commands registered IN the database itself
   via SQLink metadata tables. The wasm bytes are optionally
   **bundled** inside the db (CAS-style, same pattern the extensions
   site already uses). Move the `.db` to another machine, every
   bundled command moves with it.

A command at use time resolves through:

```
.foo args
  └─> built-in registry  (in-process map populated at startup)
       └─> miss: session-loaded extensions (.load FILE)
            └─> miss: db-resident table `sqlink_dotcmd`
                 └─> bundled  load bytes from `sqlink_artifact`
                 └─> unbundled  resolve digest via configured CAS
                      └─> miss: error
```

The same WIT contract serves both layers. The dispatcher just walks
its registry in priority order.

## Layer 1: WIT contract

See `sqlite-loader-wit/wit/dotcmd.wit` for the full file. Key
surface:

### `dot-command-spec` (in `metadata`)

Added to the existing manifest. An extension that registers any
dot commands lists them here:

```wit
record dot-command-spec {
    id: u64,
    name: string,           // "foo" for `.foo`
    summary: string,        // 1-line for `.help`
    help: string,           // multi-line for `.help foo`
    requires-write: bool,
}
```

### `dot-command.invoke` (export)

```wit
invoke: func(
    func-id: u64,
    ctx: invoke-context,
) -> result<invoke-result, sqlite-error>;

record invoke-context {
    args: string,
    interactive: bool,
    display-mode: string,
    bail-on-error: bool,
}

record invoke-result {
    text: string,
    state-deltas: list<state-delta>,
    ok: bool,
    exit-code: s32,
}
```

### Imports the extension uses

- `cli-stdout.write(text)`, `cli-stdout.flush()`, `cli-stdout.row-end()`
- `cli-stderr.write(text)`
- `cli-state.{get-text,get-int,get-bool,get-real,get-value,list-keys}`
- `spi.execute(...)` — already exists; lets the extension run SQL
  against the active connection (this is how `.tables`, `.schema`,
  etc. are implemented inside the future `core-dotcmd` extension)

### New world

```wit
world dotcmd-aware {
    import types;
    import spi;
    import logging;
    import config;
    import cli-stdout;
    import cli-stderr;
    import cli-state;

    export metadata;
    export dot-command;
}
```

## The three entry points

### 1. Interactive shell

```
sqlite> .import-json users users.json
Loaded 1234 rows into users (4 columns, 6.2 ms).
sqlite> .tables
users
sqlite> SELECT count(*) FROM users;
1234
```

Today's UX, unchanged. The repl parses lines starting with `.` and
hands `args = "users users.json"` to the dispatcher.

### 2. CLI argv subcommand

Two equivalent forms, both supported:

**Dot-prefixed (the explicit form, recommended for scripts):**

```bash
sqlink my.db .import-json users users.json
sqlink my.db .schema users
sqlink my.db .sqlink install file:///path/to/foo.wasm --bundle
```

**Git-style (the bare form, recommended for interactive shell use):**

```bash
sqlink my.db import-json users users.json    # no dot prefix
sqlink schema --db my.db users               # db as flag
```

#### Argv parsing rules

The cli's argv parser walks left-to-right:

1. Flags consumed by the cli itself: `--db PATH`, `--ro`, `--load
   FILE`, `--allow-net`, etc. These are the only "real" cli flags.
2. The first non-flag positional arg:
   - If it ends in `.db` / `.sqlite` / `.sqlite3` AND no `--db`
     was passed, treat as the db path.
   - Else: drop into command dispatch.
3. Command dispatch:
   - If the next arg starts with `.`, strip the dot and look up
     in the registry.
   - Else (bare-name form), look up directly in the registry. If
     it matches, that's the command. If not, error: "unknown
     command 'foo' (did you mean .foo or a db path?)".
4. Everything after the command name is forwarded verbatim to
   `dot-command.invoke(ctx.args = "users users.json")`. The
   extension owns its own arg parsing  the cli doesn't try to
   peek inside.
5. When the command finishes, `invoke-result.exit-code` becomes
   the process exit status. `bail/on-error` is implicitly on
   for argv-mode.

When the command line ends after the command finishes, the cli
exits. The repl never starts. That's what makes dot commands
first-class CLI tools instead of repl-only helpers.

If you want a command followed by a repl session, use
`--keep-open`:

```bash
sqlink my.db .import-json users users.json --keep-open
# (after import succeeds, drops into the interactive shell)
```

### 3. SQL programmatic API

Every registered dot command is also callable from SQL via the
`dot_command()` function family:

```sql
-- Variadic scalar form: all args coerced to TEXT, returns TEXT
-- carrying whatever the command would have streamed to stdout.
SELECT dot_command('import-json', 'users', 'users.json');

-- Variadic with structured result: returns a row per output
-- line (when the command emits to cli-stdout). Useful when the
-- output is itself tabular.
SELECT line FROM dot_command_lines('schema', 'users');

-- Same shape, different return: a JSON object with the full
-- result (text, exit_code, state_deltas applied).
SELECT dot_command_json('tables', 'main');
```

Why this matters:

- **Tooling that drives the cli over SQL** (httpd handlers,
  ORMs, scheduled jobs) gets the same surface as humans.
- **Composable**: `INSERT INTO log SELECT dot_command('sha3sum',
  ?) ...` works just like any other UDF.
- **Stored procedures**: `CREATE TRIGGER on_insert AFTER INSERT
  ON t BEGIN SELECT dot_command('sync', new.id); END;`  fully
  legal once dot commands are functions.

#### SQL surface details

| name | shape | returns |
|---|---|---|
| `dot_command(name, args...)` | scalar | TEXT (full stdout) |
| `dot_command_lines(name, args...)` | TVF | one row per line of stdout |
| `dot_command_json(name, args...)` | scalar | JSON `{ "text": ..., "exit_code": N, "ok": bool }` |
| `dot_command_caps(name)` | scalar | JSON capability descriptor |

These are themselves registered by `core-dotcmd` (or a sibling
helper extension) so the SQL surface stays consistent with the
WIT contract  if a command exists, all three entry points see it.

Calls from SQL go through the same dispatcher with
`ctx.interactive = false`. State writes (`state-deltas`) are
ignored in SQL mode unless the connection was opened with
`--allow-sql-dotcmd-state-writes`  preserves the principle of
least surprise (a query shouldn't silently change cli flags).

## Layer 2: database-resident registry

### Schema (new tables in the user's database, prefix `sqlink_`)

```sql
-- One row per registered command.
CREATE TABLE IF NOT EXISTS sqlink_dotcmd (
    name              TEXT PRIMARY KEY,         -- without leading dot
    summary           TEXT NOT NULL,
    help              TEXT,
    func_id           INTEGER NOT NULL,         -- extension's dot-command-spec.id
    requires_write    INTEGER NOT NULL DEFAULT 0,
    -- Where to find the wasm bytes:
    artifact_digest   TEXT NOT NULL,            -- blake3:...
    artifact_size     INTEGER NOT NULL,
    -- Manifest snapshot for offline introspection:
    manifest_json     TEXT NOT NULL,
    -- When the command was installed:
    installed_at      TEXT NOT NULL DEFAULT (datetime('now')),
    -- Source the command came from:
    source_uri        TEXT,                     -- 'cas:blake3:...' or 'file:...'
    -- User-set or installer-set tags.
    tags_json         TEXT NOT NULL DEFAULT '[]'
);

-- CAS for bundled wasm components. Content-addressed; one row
-- per distinct (digest, bytes) pair. Many sqlink_dotcmd rows may
-- point at the same artifact_digest (a single extension component
-- registers many commands).
CREATE TABLE IF NOT EXISTS sqlink_artifact (
    digest      TEXT PRIMARY KEY,               -- blake3:...
    size        INTEGER NOT NULL,
    bytes       BLOB NOT NULL,
    bundled_at  TEXT NOT NULL DEFAULT (datetime('now')),
    -- Provenance (best-effort; informational):
    source_uri  TEXT
);

-- Resolver config  ordered list of external CAS endpoints to
-- consult when a sqlink_dotcmd row has no matching sqlink_artifact.
CREATE TABLE IF NOT EXISTS sqlink_cas_resolver (
    priority    INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,                  -- 'file' | 'http' | 's3' | 'ipfs'
    uri         TEXT NOT NULL,                  -- e.g. 'https://cas.sqlink.io/'
    auth_json   TEXT
);
```

These tables live in the user database (the one they pass via
`--db PATH`). They're cheap when empty and don't conflict with
anything in plain SQLite.

The cli looks for them on startup; if any are missing, it skips
the db-resident layer silently (same as a session without
extensions).

### Resolution algorithm

```
dispatch(name, args):
  # 1. Session registry (built-ins + .load'd extensions).
  if name in session_registry:
      return session_registry[name].invoke(args)

  # 2. Database row.
  row = SELECT * FROM sqlink_dotcmd WHERE name = ?
  if row is None: return cmd_not_found_error(name)

  # 3. Locate the wasm bytes.
  bytes = SELECT bytes FROM sqlink_artifact WHERE digest = ?
  if bytes is None:
      # Try external CAS resolvers in priority order.
      for resolver in SELECT * FROM sqlink_cas_resolver ORDER BY priority:
          bytes = resolver.fetch(row.artifact_digest)
          if bytes is not None and blake3(bytes) == row.artifact_digest:
              break
      else:
          return cas_miss_error(row.artifact_digest)

  # 4. Verify + cache + dispatch.
  ext = load_or_get_cached(row.artifact_digest, bytes)
  return ext.invoke(row.func_id, args)
```

The cli memoizes by digest: a command from an unbundled extension
hits the resolver once per session, then stays warm in memory.

### The `.sqlink` meta-cli

The CLI ships a built-in dot command `.sqlink` whose sole job is
managing the database-resident registry. It's a sub-command
dispatcher:

```
.sqlink list                        # show all registered commands
.sqlink show <name>                 # row + manifest snippet
.sqlink install <uri> [--bundle]    # add row from a uri (file://, http(s)://, cas:...)
.sqlink uninstall <name>            # delete row (artifact stays if other rows use it)
.sqlink bundle <name>               # ensure artifact_digest's bytes are in sqlink_artifact
.sqlink unbundle <name>             # delete from sqlink_artifact (row stays; will resolve via CAS)
.sqlink bundle-all                  # convenience  bundle every row's artifact
.sqlink unbundle-all                # convenience  drop every artifact (pure metadata db)
.sqlink resolver list / add / remove / set-priority
.sqlink verify                      # re-hash every artifact row, flag mismatches
.sqlink export <name> <path>        # write the wasm bytes to disk
.sqlink gc                          # drop sqlink_artifact rows no sqlink_dotcmd references
```

A typical workflow:

```bash
# Install + bundle so the db is portable to a host without CAS.
sqlink --db work.db
sqlite> .sqlink install https://cas.sqlink.io/blake3:9a... --bundle
sqlite> .sqlink list
  json-tools         Walk + reshape JSON     (bundled, 184 KiB)
  schema-diff        Diff two schemas        (bundled, 76 KiB)
sqlite> .schema-diff main other
  ALTER TABLE users ADD COLUMN created_at TEXT;
```

Move `work.db` to another machine and `.schema-diff` keeps working
without re-fetching. To shrink the db once the commands are
elsewhere:

```sql
sqlite> .sqlink unbundle-all
sqlite> .sqlink resolver add 1 https://cas.internal/
-- Future runs resolve from internal CAS on demand.
```

## Discoverability

Every command's manifest carries `name`, `version`, `summary`,
`usage`, `help`, `examples`, `requires-write`. The same fields
power four front-ends:

### CLI flags

```bash
sqlink --list-commands              # all known commands, summary column
sqlink --list-commands --json       # machine-readable
sqlink --help-command import-json   # usage + help + examples for one
sqlink --version-command import-json
sqlink --where-command import-json  # session-loaded? db-resident? bundled?
```

### Interactive shell

```sql
sqlite> .help                       # full list, grouped
sqlite> .help import-json           # one command's details
sqlite> .help session               # category prefix expands
sqlite> .commands                   # alias for `.help` with a tighter
                                    -- header  fewer keystrokes
```

### SQL (programmatic)

```sql
SELECT name, summary, version FROM dot_command_list();
SELECT * FROM dot_command_info('import-json');
SELECT dot_command_caps('import-json');  -- declared-capabilities
```

`dot_command_list()` is a TVF backed by the runtime registry,
not a static snapshot  it includes whatever's loaded right now.

### Tab completion

The cli's tab-completer reads from `dot_command_list()`. Adding a
new dot command means `.imp<tab>` completes whether it's a
built-in, a `.load`'d extension, or a db-resident row resolved
through CAS. Same registry; same completer.

## The git-style mental model

The split mirrors what `git` does, intentionally:

| git | sqlink |
|---|---|
| `git clone URL` | `sqlink my.db .clone-from URL` |
| `git fetch` | `sqlink my.db .sync` |
| `git status` | `sqlink my.db .show` |
| `git rebase --interactive` | `sqlink my.db .schema-diff main other` |
| user-installed `git-foo` script | `.sqlink install <uri>` |
| `git --list-cmds=main` | `sqlink --list-commands` |
| `git help foo` | `sqlink --help-command foo` |
| `git config --global` | `sqlink --keep-open my.db .sqlink resolver add ...` |

Each command is one binary (here, one wasm component) implementing
one verb. The CLI shell is just a launcher that knows how to find
them and feed them argv. Same model, different transport.

The big payoff: a SQLink package manager  call it `.sqlink
install foo` or `sqlink install foo`, your choice  reuses the
exact mechanism that makes the cli surface extensible. There is
no separate "plugin protocol" to design later. The dot-command
contract IS the plugin protocol.

## Migration

### Phase 1 — WIT + session layer + shell entry point (~3 days)

- Land `sqlite-loader-wit/wit/dotcmd.wit`.
- Add `dot-commands: list<dot-command-spec>` to the manifest record,
  with the `version` / `usage` / `examples` fields.
- Add the `dotcmd-aware` world.
- Implement the host-side imports in the cli runtime
  (`cli-stdout`, `cli-stderr`, `cli-state`).
- Wire SHELL entry point: repl line starting with `.` → dispatcher.
- Write a hello-world example extension (`.greet NAME`).
- Smoke: load the example via `.load`, verify it runs and
  produces the expected output, state delta, exit code.

### Phase 1.5 — argv + SQL entry points (~2 days)

- Argv parser changes:
  - First positional ending in `.db`/`.sqlite*` → db path.
  - Next positional starting with `.` → command + verbatim
    remainder as args.
  - Bare-name form (no leading dot) → registry lookup. If miss,
    suggest `.NAME` or check spelling.
  - `--keep-open` to retain the repl after command exit.
  - `--list-commands` / `--help-command NAME` /
    `--version-command NAME` / `--where-command NAME` flags.
- SQL surface (built-in scalar + TVF, registered by `core-dotcmd`
  in Phase 2):
  - `dot_command(name, args...)` → TEXT
  - `dot_command_lines(name, args...)` → TVF, 1 row per line
  - `dot_command_json(name, args...)` → JSON
  - `dot_command_list()` → TVF over the runtime registry
  - `dot_command_info(name)` → JSON of the spec
  - `dot_command_caps(name)` → JSON of declared-capabilities
- Smoke: same `.greet` extension reachable from all three modes;
  output identical; exit codes consistent.

### Phase 2 — extract core-dotcmd extension (~5 days)

- Create `extensions/core-dotcmd/` and port today's `cli/src/dot.rs`
  commands into it one-by-one.
- The cli statically embeds `core-dotcmd` (`embed-core-dotcmd`
  feature, on by default) the same way `embed-sha3` etc. work.
- At startup the cli calls `core_dotcmd::register_into(session_registry)`
  — built-ins now ARE the first session-layer extension, not a
  separately-implemented surface.
- Keep `cli/src/dot.rs` as a back-compat dispatcher that walks the
  registry; deprecate it in a follow-up.

### Phase 3 — database registry (~5 days)

- Add the `sqlink_dotcmd` + `sqlink_artifact` + `sqlink_cas_resolver`
  tables. Detect-and-skip if absent.
- Add a `sqlink_dotcmd_registry` provider in the cli that the
  dispatcher consults after the session miss.
- Ship the `.sqlink` meta-cli as its own dot-command extension
  (in `extensions/sqlink-meta-cli/`). It's the canonical user of
  the new tables and a useful litmus for the contract.
- File-URI installer: `.sqlink install file:///path/to/foo.wasm`
  fingerprints the bytes, inserts into both tables.
- HTTP installer: same shape, fetches first.

### Phase 4 — external CAS (~5 days)

- File-CAS resolver: `kind = 'file', uri = '/path/to/cas-root'`.
  Probes `cas-root/blake3/<first-2>/<rest>`.
- HTTP-CAS resolver: `kind = 'http', uri = 'https://cas.../'`.
  HEAD-checks digest path, GETs on hit.
- Capability gating: resolvers respect the same `--allow-net` /
  `--allow-fs` flags as the rest of the cli. Default: file
  resolvers OK in interactive mode; http resolvers require
  explicit opt-in.

### Phase 5 — cleanup (~2 days)

- Remove `cli/src/dot.rs` entirely; everything now flows through
  the registry.
- Document the state schema in `tooling/cli-cheatsheet.md`.
- Write `AUTHORING-DOTCMD-COMPONENTS.md` alongside the existing
  `AUTHORING-RUN-COMPONENTS.md`.

## State schema (Layer 1 — what `cli-state` exposes)

Slash-namespaced keys. The cli treats this as its source of truth
during a dispatch; mutations flow through `state-deltas` in the
invoke result.

| key                    | type      | meaning |
|------------------------|-----------|---------|
| `display/mode`         | text      | "list", "column", "csv", "json", "insert", "html", "markdown", "tcl" |
| `display/headers`      | bool      | print column headers? |
| `display/nullvalue`    | text      | text printed for NULL |
| `display/separator`    | text      | column / row separator |
| `display/width`        | text      | space-separated per-column widths |
| `io/echo`              | bool      | echo commands before running |
| `io/output`            | text      | output file ("" = stdout) |
| `io/timer`             | bool      | print wall-time after each statement |
| `io/changes`           | bool      | print row-change counts |
| `stats/enabled`        | bool      | `.stats on/off` |
| `stats/explain`        | text      | "off" | "on" | "auto" |
| `bail/on-error`        | bool      | abort on first error |
| `binary/on`            | bool      | render BLOBs as bytes vs hex |
| `parameter/<name>`     | sql-value | `.parameter set X Y` bindings |
| `db/path`              | text      | current main-database path |
| `db/readonly`          | bool      | |
| `db/changes-total`     | s64       | SQLite `total_changes()` snapshot |
| `prompt/main`          | text      | `.prompt MAIN` |
| `prompt/cont`          | text      | `.prompt MAIN CONT` |

The cli accepts state-deltas only on this allowlist; unknown keys
get a non-fatal warning and are dropped.

## Capability model

A dot-command extension declares the same `capability` set as
any other extension (manifest's `declared-capabilities`). The
new capability values for the dot-command surface:

- `cli-stdout` — write to the cli's output. Almost every dot
  command needs this; granted by default unless the user passes
  `--strict-caps`.
- `cli-stderr` — granted alongside `cli-stdout`.
- `cli-state-read` — read session state. Granted by default.
- `cli-state-write` — apply state-deltas. Granted by default for
  built-ins; opt-in (`--allow-dotcmd-state-write`) for user
  extensions to prevent surprise mutations.

The existing `db-readonly` / `db-mutate` capabilities continue to
gate SQL execution as before; `requires-write` on the spec
short-circuits dispatch when the db is readonly.

### Mode-specific grants

The same command runs through three transports; capabilities
default DIFFERENTLY per mode:

| capability | shell | argv | SQL |
|---|---|---|---|
| `cli-stdout` | ✓ | ✓ | filtered  routed to result buffer |
| `cli-state-read` | ✓ | ✓ | ✓ |
| `cli-state-write` | ✓ (built-ins) / opt-in (user) | ✓ (built-ins) / opt-in (user) | ✗ unless `--allow-sql-dotcmd-state-writes` |
| `db-mutate` | ✓ if not `--ro` | ✓ if not `--ro` | ✓ if not `--ro` AND `requires-write` matches |
| `net` / `fs` (host caps) | as usual | as usual | as usual (no auto-grant) |

Rationale: a `SELECT ...` query the user runs from a web handler
shouldn't have side-channel paths to flip `.timer on` or `.bail
on` invisibly. argv-mode behaves like an interactive shell — same
user, same intent.

## Collision policy

Three sources collide on names:

1. Built-ins (the preloaded `core-dotcmd` extension)
2. Session `.load`'d extensions
3. db-resident `sqlink_dotcmd` rows

Default precedence: **built-ins win, then session, then db**.
The user can flip this with `--dotcmd-precedence db,session,builtin`
or per-command via `.sqlink prefer foo db` (writes a flag row).

For shipping: built-ins always win for the commands that ship in
`core-dotcmd`. If a user defines `.tables` in an extension, it's
shadowed by the built-in; calling it directly fails with a clear
"shadowed by built-in" message and a hint to use `.tables@user`.

## Open questions / risks

- **Bidirectional stdin streaming.** Commands like `.read FILE`
  pipe lines into the parser. v1 leaves this as a built-in (the
  parser does the read directly); pluggable stdin can come later.
- **Long-running commands.** `.dump` on a 5 GB db streams for
  minutes. The WIT call is sync — we need to make sure
  `cli-stdout.write` flushes through to the terminal as it runs,
  not just at the end of `invoke`. The host implementation must
  not buffer indefinitely.
- **Crash isolation.** A faulty user dot-command shouldn't crash
  the cli session. We already run extensions in wasmtime stores
  with per-call traps; same applies here. The dispatch catches
  the trap, prints `[dotcmd: trap in .foo: <msg>]` to stderr,
  and returns to the prompt.
- **State write conflicts.** Two extensions could try to set
  the same key with conflicting deltas. Last-write-wins is the
  obvious policy; document it.
- **Backwards compatibility.** Existing tooling (smoke tests, the
  bench harness) uses today's built-in `.tables`/`.schema`/etc.
  After Phase 2, these still work — they're just delivered via
  the registry now.
- **DB size with bundled extensions.** 50 bundled commands at
  ~200 KiB each is ~10 MiB added to every db. Acceptable for
  desktop use; surface it in `.sqlink list`. Power users will
  unbundle and point at a CAS.
- **Migration drift.** If a user upgrades the cli to a version
  with different built-ins, db-resident rows with the same name
  silently get shadowed. Make `.sqlink list` flag this with a
  `(shadowed)` tag.

## Test plan

- Per-phase smoke tests in `tooling/smoke.py`:
  - Phase 1: hello-world `.greet` extension; verifies dispatch +
    `state-delta` round-trip via the SHELL entry point.
  - Phase 1.5: same extension, exercise the OTHER TWO entry
    points  argv (`sqlink :memory: .greet world`) and SQL
    (`SELECT dot_command('greet','world')`). All three return
    identical bytes. `dot_command_list()` reports `.greet`.
  - Phase 2: every built-in passes its existing smoke after the
    move to `core-dotcmd`. Diff against pre-move output.
  - Phase 3: install a file:// extension, run it, restart the
    cli, verify it persists. Bundle it, copy the db elsewhere,
    verify it still runs.
  - Phase 4: install from CAS, unbundle, kill the cli, restart
    without network, verify graceful failure with the right
    error.
- Capability-gate tests: a malicious dot-command that tries to
  set `db/path` to `/etc/passwd` is rejected; one that writes to
  a file path via `io/output` while `--allow-fs` is off is
  rejected; etc.
- Performance: `.dump` on a 100 MB db is no slower after the
  refactor than before (measured via `bench.py`). The wasm
  boundary should not dominate for streamed output.

## Why this is worth doing

- **Decouples the cli surface from the cli release cadence.**
  New commands ship without a CLI rebuild.
- **Database-resident commands make `.db` files self-describing.**
  Ship the db and the commands come along. Big for portable
  analytics tooling, classroom material, debugging artifacts.
- **The built-ins/plugins distinction collapses.** Every command
  goes through the same path; the security/extensibility/
  testability story is one story, not two.
- **CAS resolution + bundling reuses infrastructure we already
  have** (the extensions site does the same dance for component
  artifacts). Net new code is small.
- **It composes with everything else SQLink.** A user can
  install a dot command from the same registry that serves
  extensions; future "package manager" tooling sees one surface.
