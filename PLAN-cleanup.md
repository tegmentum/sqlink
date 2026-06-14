# Plan: Outstanding cleanup after the reactor → command + lib split

> **Status: phases 0-5 shipped; 6-7 remain.**
>
> | Phase | Commit |
> |---|---|
> | 0 — Investigate | 181efda |
> | 1 — Doc cleanup | 216bd34 |
> | 2 — SPI/high-level state alignment | cdbc047 |
> | 3 — Rename library world | 8c87063 |
> | 4 — Workspace Cargo.toml | e7b6298 |
> | 5 — Legacy world status | f11edf1 |
> | 6 — Runnable-uses-sqlite-lib demo | (open) |
> | 7 — Shell wrapper integration test | (open) |
>
> Phase 2's persistence bug, which I originally framed as "the
> shared default connection across SPI + high-level," turned out to
> involve a second latent issue (the wasm component receiving
> `:memory:` instead of `--db`'s value); the actual fix landed in
> cbb5761. Phase 6's "Fiji-uses-sqlite-lib" wording is stale — the
> Fiji concept was renamed to "runnable component" in 8d879b0, so
> the modern phrasing is "runnable-uses-sqlite-lib."

Scope: the eleven items surfaced after Phase 1 (cli command-mode),
Phase 2 (sqlite-lib + core split), the rename, and the
programmatic-load-extension work. Excludes pre-existing plans
(`PLAN-outstanding.md` is a stale snapshot of work that's now
done; `PLAN-cli-commands.md` is unrelated dot-command expansion).

Phases are ordered for dependency, not priority. Each phase is one
commit unless noted. Stop after any phase — they don't cascade.

---

## Phase 0 — Investigate (done; no commit beyond `git add`)

**0.1 — `PLAN-cli-commands.md` status.** Read end-to-end: it's a
live future-work plan for adding ~22 missing dot-commands to the
**C** CLI (`src/cli/sqlite_cli.c`), not the Rust `cli/` crate.
Different codebase area. Action: `git add PLAN-cli-commands.md`
so it stops being untracked; no other change.

**0.2 — Legacy/unified worlds, live consumers.** Both legacy C
worlds are load-bearing:

- `sqlite-cli-unified` (`wit/unified-world.wit`):
  - `Makefile` 169–582 — `wit-bindgen c --world sqlite-cli-unified`,
    compiles `sqlite_cli_unified.wasm`
  - `src/cli/sqlite_cli.c` includes `sqlite_cli_unified.h`
  - `src/exports/extension-unified.c` is host glue
  - `host/README.md`, `host/src/lib.rs` documentation references
- `sqlite-cli` (legacy, declared in **both** `wit/world.wit:28`
  and `wit-cli/cli.wit:4`):
  - `Makefile` builds `build/sqlite-cli.wasm` from
    `src/cli/sqlite_cli.c` — likely against one of these two
    declarations; need to verify which.

**Consequence for Phase 5:** the "if no live consumer, delete"
framing was wrong — every world has C consumers. Phase 5 below is
rewritten accordingly.

---

## Phase 1 — Doc + dead-file cleanup (one commit)

Mechanical sweep, no behavior change.

1. `git rm PLAN-reactor-cli-async-host.md` — entirely obsolete; the
   reactor world is gone, the host crate's `pub mod reactor` is
   gone, the architectural decision is captured in
   `host/SPI-LIVE-ARCHITECTURE.md`.
2. Sweep `cli-rust` → `cli` and `lib-rust` → `sqlite-lib` across:
   - `ARCHITECTURE.md`
   - `CI.md`
   - `AUTHORING-FIJI-FUNCTIONS.md`
   - `cli/README.md`
   - `PLAN-outstanding.md` — or mark `**Superseded by Phase 1 of
     PLAN-cleanup.md (committed cN)**` at the top and stop editing.
3. Reframe `host/SPI-LIVE-ARCHITECTURE.md`. The technical content
   (may_enter spec rule, option A/B/C analysis, declarative
   data-dependency manifest path) is still correct. Re-cast the
   prose from "what we plan to do about the bridge" to "what we
   learned from tearing the bridge out — keep this doc so the
   architectural conclusion survives if anyone proposes
   live-SPI-style re-entry again."

Sanity check: `grep -rln "cli-rust\|lib-rust"` returns nothing in
`*.rs`, `*.toml`, `*.wit`, or any non-snapshot `*.md`.

---

## Phase 2 — Fix the SPI/high-level state split in sqlite-lib
(latent bug, design decision needed, one commit)

Today: `spi.execute()` opens its own thread-local in-memory
connection in a `SPI_CONN` thread-local. `high_level.open_memory()`
and `high_level.open_file(path)` each create their own
`HlConnection` resource with its own connection. A consumer that
calls `high_level.open_memory()`, runs `CREATE TABLE t(...)`, then
calls `spi.execute("SELECT * FROM t")` sees an empty database
because SPI is talking to a different sqlite3 connection.

This is wrong as written. Three options:

- **A. Shared default connection.** SPI's thread-local connection
  becomes a `Rc<RefCell<db::Connection>>`. `HighLevel` gains a
  `default_connection() -> Connection` export that hands out a
  resource wrapping the same `Rc`. Consumers that want shared
  state use the default; consumers that want isolation call
  `open_memory()` / `open_file()` and ignore SPI for that path.
  Doesn't break the existing resource-per-connection model.
- **B. Document the gap.** Top-of-`lib.rs` doc-comment + a WIT
  comment on `library` that SPI and high-level are independent
  connection pools by design. Cheap, ships a footgun.
- **C. Route SPI through the most-recently-opened high-level
  connection.** Implicit, magic, surprising — skip.

**Pick A.** B is a footgun; C is wrong. Implementation sketch:
```rust
// sqlite-lib/src/lib.rs
thread_local! {
    static DEFAULT_CONN: RefCell<Option<Rc<RefCell<db::Connection>>>>
        = const { RefCell::new(None) };
}
fn default_conn() -> Rc<RefCell<db::Connection>> {
    DEFAULT_CONN.with(|c| {
        let mut g = c.borrow_mut();
        if g.is_none() {
            *g = Some(Rc::new(RefCell::new(db::Connection::open_in_memory().unwrap())));
        }
        g.as_ref().unwrap().clone()
    })
}
// SpiGuest::execute / execute_scalar / execute_batch route through default_conn()
// HighLevelGuest gets a new method: default_connection() -> Connection
//   returning an HlConnection { conn: default_conn() }
```

WIT change:
```wit
// wit/sqlite-high-level.wit, inside interface high-level
default-connection: func() -> connection;
```

Add a smoke test in `host/tests/sqlite_lib.rs` that asserts
SPI sees high-level writes through `default_connection()`.

---

## Phase 3 — Rename the library world (one commit)

`sqlite-cli-library` → `sqlite-library`. The `-cli-` infix is
dead weight; the world doesn't expose anything CLI-shaped.

Touches:
- `wit/unified-world.wit` — `world sqlite-cli-library { ... }`
  declaration + doc-comment references
- `sqlite-lib/src/lib.rs` — `wit_bindgen::generate!({ world:
  "sqlite-cli-library", ... })` → `"sqlite-library"`
- `host/tests/sqlite_lib.rs` — `wasmtime::component::bindgen!({
  world: "sqlite-cli-library", ... })` → `"sqlite-library"`. The
  generated type name `SqliteCliLibrary` becomes `SqliteLibrary`
  — sweep usages.
- `wit/library.wit` — doc-comment mentions of the world name.

No behavior change. Verify all 23+ host tests pass.

---

## Phase 4 — Workspace Cargo.toml (one commit, possibly skip)

Add a root `Cargo.toml`:
```toml
[workspace]
members = ["core", "cli", "sqlite-lib", "host"]
resolver = "2"
```

**Caveat to check first:** `cli/.cargo/config.toml` and
`sqlite-lib/.cargo/config.toml` both set `target = "wasm32-wasip2"`
as the default. Cargo config inherits up the dir tree at invocation
time, not down — so `cargo build --workspace` from the root
finds no `.cargo/config.toml` and uses the native target, which
would try to compile `libsqlite3-sys` natively for the wasm
crates. That'll likely fail.

Two workable shapes:

- **A.** Add a root `.cargo/config.toml` with no default target;
  the per-crate ones still override when you run from inside the
  crate. Workspace builds skip the wasm crates (mark them as
  `[workspace.exclude]` or use `default-members = ["host", "core"]`).
- **B.** Skip the workspace entirely. Document the build flow in a
  top-level `BUILD.md`. Mostly bookkeeping, no real upside without
  workspace-level features wanted.

**Recommend A** if it works cleanly, otherwise skip. Verify on
fresh clones; this is the kind of change that breaks subtly.

---

## Phase 5 — Legacy world status (don't delete; clarify)

Phase 0.2 found both legacy C worlds (`sqlite-cli`,
`sqlite-cli-unified`) have live consumers in the C build path. We
can't delete them without retiring the C CLI binaries, which is
out of scope.

What we *can* do:

1. Resolve the duplicate `sqlite-cli` world declaration. It exists
   in both `wit/world.wit:28` and `wit-cli/cli.wit:4`. One of
   those is dead. Verify which by checking what `wit-bindgen c`
   actually loads when building `build/sqlite-cli.wasm`, then
   delete the unused declaration.
2. Update doc comments on the surviving legacy worlds. The
   `wit/unified-world.wit` doc-block currently says the legacy
   `sqlite-cli` world "stays for the imperative-register build
   path until that path is retired" — that statement is still
   true; leave the prose alone, but add a one-line cross-reference
   to where the C CLI source lives (`src/cli/sqlite_cli.c`,
   built via the Makefile).
3. Note in `wit/world.wit` the same — point readers at the C build
   path so they know what's consuming the world.

Retiring the C CLI binaries (and thus the legacy worlds) is a
separate, much larger project — gated on the Rust CLI reaching
dot-command parity (see `PLAN-cli-commands.md` for the missing
~22 commands, even though that plan is C-targeted; the same gap
applies to the Rust crate). Not in scope here.

---

## Phase 6 — Fiji-uses-sqlite-lib demo (new work, one commit)

Validates the SPI port end-to-end and gives a concrete pattern for
"how do I use sqlite-lib from a real component?"

- `extensions/fiji-sqlite-demo/` (or similar) — a Fiji function
  component that imports `sqlite:extension/spi@0.1.0`.
- Compose script that wires the Fiji function's SPI import to
  `sqlite-lib`'s SPI export.
- Integration test in `host/tests/`:
  1. Build the Fiji function.
  2. Compose Fiji + sqlite-lib into a single binary.
  3. Instantiate via the host's existing Fiji runner.
  4. Assert the Fiji function's output reflects rows written by
     SPI calls.

Blocked by Phase 2 (need the SPI/high-level alignment landed so
the Fiji function can see writes from any high-level consumer that
shares the connection). Blocked by Phase 3 if the rename happens
(the demo's WIT references will need the new world name).

---

## Phase 7 — Shell wrapper integration test (one commit)

`tests/cli/sqlite-wasm.sh` (or similar) — bash script that
exercises every invocation shape of `./sqlite-wasm`:
- bare interactive (pipe `.quit` in)
- `:memory:` DB
- file-backed DB (mktemp)
- one-shot `DB "SQL"`
- stdin pipe of multi-statement SQL
- `--` passthrough

Each shape asserts on stdout. The bash script is invoked from a
Rust integration test under `host/tests/` so it runs as part of
`cargo test`. Skips gracefully if `cli/.../sqlite_cli.component.wasm`
isn't built.

---

## Commit checklist

Each phase ships its own commit. Conventional-commits format, no
Claude/Anthropic refs, no emojis. Phase 1 is a single commit even
though it touches many files because none of the changes are
independently meaningful. Phase 2 is the only one that should land
with a runtime regression test.

Suggested ordering for one sitting:
0 → 1 → 3 → 2 → 4 → 5 → 6 → 7

Phase 3 before 2 because the rename is mechanical and unblocks 2
without contaminating the diff with renames. Phase 5 before 6 so
the demo doesn't accidentally reference a world that's about to
get deleted.
