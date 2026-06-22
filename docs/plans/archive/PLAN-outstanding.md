# Plan: Outstanding Work

> **Status: superseded.** This document is a snapshot from when the
> Rust CLI was still a reactor-shape component with a live-SPI
> bridge. That entire architecture has since been torn out (see
> `host/SPI-LIVE-ARCHITECTURE.md` for the post-mortem) and the
> reactor world replaced with command-mode `wasi:cli/run`. The
> "missing" items listed here are either done or no longer
> applicable; current cleanup work lives in `PLAN-cleanup.md`.
> Kept for historical reference; do not act on its contents.

## State of the world

What's working today end-to-end:

- `sqlite-cli-demo.wasm` (legacy C, wasi:cli command-mode) — full
  dot-command coverage, .load + dispatch for scalar/aggregate/
  collation/hooks. Used by the existing `make cli-demo-test` path.
- `sqlite_cli_rust.wasm` (Rust reactor, async lifts) — minimal
  dot-commands (.load .quit .exit), file-backed db with --db flag,
  hybrid SPI via host-side helper connection. The architectural
  groundwork for in-WASM SPI is real and proven by
  `SELECT wasm_table_count('t')` reading committed user data.

What's not yet built:

- The Rust reactor CLI's dot-command surface (everything except
  .load/.quit/.exit) — biggest gap to making it user-usable
- spi.execute_live (the "live" half of hybrid SPI)
- Resolver components + CAS cache (separate plan
  PLAN-resolvers-and-cas.md, untouched)
- Graceful failure for `wasmtime run` against the reactor binary
- Granular `.load-with-policy` (today's .load grants Spi + State +
  Cache by default — too permissive)
- A few smaller items called out below

## Four tracks, priority order

### Track A — Finish the Rust CLI rewrite (highest priority)

The reactor CLI doesn't have the dot-commands users will reach for.
This is the gap between "architecture works" and "shipping".

#### A1 — Port dot-commands (~3-5 days)

Current C CLI implements these (per PLAN-cli-commands.md); each
needs a Rust counterpart routing to the shared rusqlite::Connection
on the cli-rust side.

Tier 1 (must-have, user reaches for them daily):
- `.schema ?TABLE?` — query `sqlite_master`, print CREATE statements
- `.tables ?PATTERN?` — list tables matching pattern
- `.indexes ?TABLE?` — list indexes
- `.databases` — list attached databases
- `.show` — print current settings (mode, headers, etc.)
- `.headers on|off` — toggle header rows
- `.mode <mode>` — switch output mode (list/column/csv/line/table/box)
- `.nullvalue STRING` — what to render for NULL
- `.separator STRING` — column separator for `list` mode
- `.echo on|off` — echo SQL before executing
- `.prompt MAIN CONT` — change prompts
- `.print STRING...` — print arguments verbatim
- `.help` — concise listing
- `.open ?FILE?` — switch to a different database mid-session
- `.bail on|off` — stop on first error
- `.unload NAME` — symmetric to .load; calls
  extension-loader.unload-extension + clears registered fns

Tier 2 (nice-to-have, defer if context is tight):
- `.archive` — wired via zip-operations import (the C side has it)
- `.stats on|off` — print exec stats
- `.dump ?TABLE?` — emit SQL dump of table(s)
- `.read FILE` — execute SQL from file (needs WASI preopen for the
  file's dir, mirroring --db)
- `.import / .save / .clone` — file-format ops

Tier 3 (low priority):
- `.eqp on|off` — explain query plan
- `.timer on|off` — time queries

Implementation pattern, one file `cli-rust/src/dot.rs`:

```rust
pub fn dispatch(input: &str) -> Option<String> {
    let mut parts = input.splitn(2, char::is_whitespace);
    let cmd = parts.next()?;
    let arg = parts.next().unwrap_or("");
    match cmd {
        ".schema" => Some(cmd_schema(arg)),
        ".tables" => Some(cmd_tables(arg)),
        // ...
        _ => None,
    }
}
```

`eval` checks `dispatch` before falling through to SQL execution.

#### A2 — Output formatter (~1-2 days)

The C CLI's output mode tracks across statements (set via .mode,
applies until next .mode). Port to a `cli-rust/src/format.rs` module
that takes column names + rows and emits according to current mode:

- `list`: pipe-delimited (today's default)
- `column`: aligned fixed-width columns
- `line`: one value per line, `colname = value`
- `csv`: RFC 4180 with quoting
- `table`: ASCII box-drawing
- `box`: Unicode box-drawing
- `markdown`: pipe table
- `quote`: SQL-literal quoting
- `tabs`: tab-separated
- `json`: array-of-objects

Pure formatting code, no SQLite interaction. Smallest unit-testable
piece in the project.

#### A3 — Multi-statement parsing (~1 day)

Today's `is_statement_complete` accepts anything ending in `;` or
starting with `.`. Real SQLite CLI walks the input looking for:
- Unterminated string literals (`'foo`)
- Unterminated comments (`/* foo`)
- Unbalanced parens in CREATE-statement bodies
- Triggers / views that contain semicolons inside their body

Port from C side or use sqlite3's own `sqlite3_complete()` via
rusqlite. The latter is two lines and matches sqlite's CLI exactly.

#### A4 — Graceful failure stub (~1 hour)

`wasmtime run sqlite_cli_rust.wasm` today produces nothing
informative — the component is a reactor, no main. Add a synthetic
`_start` (via inline asm or just a panic in init that prints):

```
This is a reactor component. Use:
  sqlite-wasm-run --reactor sqlite_cli_rust.wasm
```

cargo-component might not allow custom `_start` easily. Fallback:
wrap a wasi:cli/run export that immediately prints + exits.

#### A5 — `.load-with-policy` (~2 days)

Today `do_load` in cli-rust hard-codes `grant: [Spi, State, Cache]`.
Real interactive usage needs:

```
.load <path> --grant=spi,state --allowed-hosts=example.com \
             --fuel-per-call=1_000_000 --epoch-ms=5000
```

Parse the trailing args, build a LoadOptions, pass through.

Also: the SPI capability today grants access to the user's *whole*
database. A future refinement (out of scope here) is read-only vs
read-write SPI as separate caps.

#### A6 — Statement (high-level) trait bodies (~1 day)

`cli-rust/src/lib.rs::HlStatement` currently stubs every method
(execute returns 0 changes, query returns empty, step returns None).
This blocks any user of the high-level Statement resource. Port
rusqlite-backed implementations following the pattern in
HlConnection.

### Track B — Complete SPI hybrid

The helper-connection path ships today (committed-state visibility).
The "live" half (outer-uncommitted visibility) remains. Defer until
a real extension demands it; the architecture leaves room.

#### B1 — spi.execute_live (~3-5 days)

Add `execute_live` to `sqlite:extension/spi` WIT. LoadedState's impl
threads a handle to the cli reactor's instance + store and calls
`cli.eval_structured` via the async-stackful lift. Validates the
reentrancy mechanism wasmtime 45 makes possible.

Mechanics:
1. Add `cli_reactor_handle: Option<Arc<Mutex<(Store, Instance)>>>`
   on `Host`. Set at sqlite-wasm-run startup.
2. LoadedState clones the handle. In its async `execute_live`:
   lock the mutex, call `instance.sqlite_wasm_cli().call_eval_structured(...)`.
3. Convert query-result between the two bindgen type universes.

Risk: the Mutex contends across nested calls. Probably fine for the
synchronous-feeling REPL but if multiple extensions concurrently SPI,
might deadlock. Document the contention model.

#### B2 — Connection pooling for SPI (~1 day)

Currently every spi.execute opens + drops a rusqlite::Connection.
Acceptable for v1 but each open() reads page 1 of the file. Pool one
Connection per loaded extension; reuse across calls.

#### B3 — In-memory db SPI workaround (~2 days)

Today `:memory:` returns error from spi.execute (rusqlite's in-memory
dbs aren't sharable across connections). Two options:

- Switch to `file::memory:?cache=shared` mode — SQLite's shared-cache
  in-memory dbs ARE sharable across connections in the same process.
  Requires the cli and host to coordinate the same URI.
- Just document the limitation: SPI requires `--db <path>` to be a
  file path.

Recommendation: document. The shared-cache route is fiddly and
prevents :memory: from being truly isolated.

### Track C — Resolver components + CAS cache

Tracked separately in `PLAN-resolvers-and-cas.md`. Five sub-steps,
~6-8 days total. Highest user-visible value (`.load https://…`) but
no dependency on Track A/B; can run in parallel.

Summary of that plan's steps:
- C1: WIT for `resolver` interface + `resolving` world
- C2: CAS cache module in host (blake3, XDG default, env override)
- C3: Host glue — register_resolver / resolve_uri / load_from_uri
- C4: http-resolver test extension (component from day 1)
- C5: CLI surface — .load <uri>, .register-resolver, .resolvers

### Track D — Polish, tests, ecosystem

Lower priority but accrues debt if ignored.

#### D1 — Async test updates (~1 day)

`host/tests/spi.rs` and `host/tests/load.rs` use sync wasmtime APIs.
Convert to `#[tokio::test]` + async instantiation. ~19 tests.

#### D2 — Existing extensions revalidated under cli-rust (~half day)

test-extension, agg-extension, coll-extension, hook-extension all
work under sqlite-cli-demo.wasm (legacy C). Verify they still work
under cli-rust:

```
$ sqlite-wasm-run --reactor --db /tmp/test.db sqlite_cli_rust.wasm
sqlite> .load test_extension.wasm
sqlite> SELECT reverse('hello');  -- should print "olleh"
```

#### D3 — Documentation pass (~1-2 days)

- A "getting started" walkthrough in README that includes the
  reactor mode invocation
- Architecture doc explaining the two CLIs (legacy C vs reactor
  Rust), when to use each
- Update PLAN-dispatch-followups.md and PLAN-reactor-cli-async-host.md
  with "complete" markers
- Capture the wasmtime-45-can't-reenter-sync-lifts finding in a
  separate ARCHITECTURE.md note for future reference

#### D4 — Sunset path for legacy C CLI (defer)

Once cli-rust has Tier 1 + Tier 2 dot-commands working and at
least one user has migrated, deprecate sqlite-cli-demo.wasm. Don't
delete — keep it as a comparison point and for users who specifically
want command-mode behavior. Just stop adding features there.

## Recommended order

```
Track A1 (Tier 1 dot-commands) ──┐
Track A2 (output formatter)      ├→ Reactor CLI is usable
Track A3 (statement parsing)     ┘    ↓
                                      Track A4 (graceful failure)
                                      Track A6 (statement bodies)
                                      Track A5 (.load-with-policy)
                                            ↓
                                      Track D1 (async tests)
                                      Track D2 (revalidate extensions)
                                      Track D3 (docs)

   In parallel, at any time:
   Track B1 (spi.execute_live)
   Track B2 (connection pooling)
   Track C  (resolver + CAS)
```

Rough total: **Track A 7-10 days; Track B 1-2 weeks (with B1 the
bulk); Track C 6-8 days; Track D 3 days.** Pick which tracks
matter most.

## Risks worth flagging

- **Track A1's surface area is wide.** Each dot-command is small
  but there are ~25 of them in Tier 1+2. Easy to underestimate the
  cumulative time. Mitigation: ship in chunks of 4-5 commands
  per commit; don't wait for all to land before merging.
- **Track B1 reentrancy semantics.** Even with async lifts, nested
  cli.eval calls might surprise us in ways the small test didn't
  catch (cursors in WAL mode, transactions, savepoints). Allocate
  time for "what breaks when an extension SPI-runs during a SELECT
  iteration" investigation.
- **Track A2's output formatter has many modes.** The "real" CLI's
  column-mode auto-sizing logic is non-trivial. Acceptable to ship
  list/csv/json first and add the visual modes incrementally.
- **Track A and Track C touch the same .load command.** A5 (policy)
  and C5 (URI loading) both modify the .load grammar. Coordinate
  the syntax before either lands so we don't ship incompatible
  versions.

## Out of scope (named explicitly so they're not assumed)

- Removing the C CLI / sqlite-cli-demo.wasm path
- Migrating extensions from the legacy ABI to anything new
- Browser deployment of cli-rust (interesting but separate plan)
- WASIp3 / async wasi:cli once it ships (not yet upstream stable)
