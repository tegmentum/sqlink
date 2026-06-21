# Phase 5 — dot.rs deletion readiness

Status as of this commit: Phases 1–4 of `PLAN-dotcmd-plugins.md`
have landed; Phase 5 has shipped its docs portion plus follow-up
commits that exercised lift #1 (widened the dotcmd-aware world
for extension-loader access via `loader-bridge`) and the
beginning of lift #2 (spi expansion for `list-vfs` + `vfs-name`).
The remaining Phase 5 deliverable — "remove `cli/src/dot.rs`
entirely; everything now flows through the registry" — is bigger
than the plan estimate suggested. This file is the inventory of
what still lives in `dot.rs` and `lib.rs`'s `eval_input`
strip-prefix chain, with the migration shape for each.

## Migrated since this doc landed

  - `.sqlink` and all subcommands — moved to
    `extensions/sqlink-meta-cli` after `loader-bridge` shipped
    (FU-1..4).
  - `.width` — `display/width` delta (FU-5).
  - `.timeout` — `conn/busy-timeout` delta applied to the cli's
    connection (FU-5).
  - `.vfslist`, `.vfsname` — via new `spi.list-vfs` +
    `spi.vfs-name` (FU-6).
  - `.show` — cli-state snapshot pushed on every dispatch;
    extension reads via `cli_state.get_*` (FU-7).
  - `.limit`, `.dbconfig` — snapshot pushes live
    `conn/limit/<name>` and `conn/db-config/<name>` values;
    set side emits matching state-deltas the cli applies on
    its own connection (FU-8).
  - `.sha3sum` — own extension (`extensions/sha3sum-cli`) using
    `spi.execute` to walk schema + rows, hashing with Sha3_256
    (FU-9).
  - `.parameter` — `params/clear` + `params/set/<name>` +
    `params/unset/<name>` deltas; `params/value/<name>` entries
    in the snapshot (FU-10).
  - `.serialize` / `.deserialize` — own extension
    (`extensions/serialize-cli`) using a new `spi.serialize-db`
    for the read and a `conn/deserialize/<name>` delta carrying
    `SqlValue::Blob` for the write. `sql_value_to_json` now
    encodes blobs as `X'<hex>'` literals to round-trip raw
    bytes through state-deltas (FU-11).
  - `.archive` — own extension (`extensions/archive-cli`) using
    `spi.execute` for sqlar table ops + `std::fs` for files +
    `miniz_oxide` for zlib (FU-12). `--list` / `--extract` /
    `--create` / `--update` all work; `--file SEPARATE_DB` is
    unsupported pending a spi.open-other-db addition.

## What's left in the cli's hard-coded dispatch

### `cli/src/dot.rs` (1 arm remaining)

| Command         | Blocker / migration target |
|-----------------|----------------------------|
| `.session`      | **structurally bound to the cli** — `sqlite3_session_create` returns a `sqlite3_session*` that tracks changes on the connection it was created against. The cli's main connection lives inside the cli wasm component; the extension's `spi` connection is a different handle. Moving session handles into an extension would track changes on the wrong connection. Fixing this needs either (a) the cli's main connection threaded into the extension's spi (sharing the raw sqlite3 handle across a wasm boundary), or (b) a new `spi.session-*` family that the cli implements as host-importable callbacks (host→guest→cli call chain). Both are non-trivial enough to defer to a follow-up. For now `.session` is the only remaining dispatch arm in `dot.rs`. |

### `cli/src/lib.rs` strip-prefix chain (~27 arms)

These never lived in dot.rs because they touch sqlink-level
state (load policy, cache, log target, grants) before the
connection is even open, or they edit settings that the load-
chain pre-reads:

| Command(s)                            | Why it can't move yet |
|---------------------------------------|------------------------|
| `.load` / `.unload` / `.reload`       | needs extension-loader; pre-conn |
| `.open`                               | switches CLI_CONN — extension can't reassign it |
| `.run`                                | invokes a runnable; needs loader |
| `.register-resolver` / `.unregister-resolver` | loader |
| `.register-runtime` / `.unregister-runtime`   | loader |
| `.register-provider`                  | loader |
| `.cache …`                            | extension-loader's CAS cache API |
| `.read FILE`                          | recursive eval_input; can't move into wasm |
| `.output`, `.once`                    | cli's `write_output` sink; extension would need a redirect host call |
| `.import FILE TABLE`                  | file IO — sandbox boundary |
| `.dump` / `.backup` / `.restore` / `.save` / `.clone` | sqlite3_backup; file IO |
| `.trace on/off/FILE`                  | host trace callback wired before sqlite3_initialize |
| `.auth on/off`                        | sqlite3 authorizer hook on the cli's connection |
| `.log`                                | host log callback installed pre-sqlite3 |
| `.grants` (and subcommands)           | grants db lives in the cli's home dir; pre-conn |
| `.compose`                            | compose CLI surface; loader |
| `.exit` / `.quit`                     | flips DONE flag in cli; impossible from wasm |

## Path to true deletion

Three architectural lifts unblock the rest:

1. **Widen `dotcmd-aware` to import `extension-loader`.**
   Unblocks `.sqlink`, `.load`, `.unload`, `.run`,
   `.register-*`, `.cache`, `.grants`, `.compose`. Once shipped,
   `.sqlink` migrates from `cli/src/dot.rs` to
   `extensions/sqlink-meta-cli/` as the plan envisioned.

2. **Expand the `spi` surface.** Add `busy-timeout`, `limit`,
   `db-config-get/set`, `list-vfs`, `vfs-name`,
   `db-config-print-bool`, plus a `cli-state.get-*` read path.
   Unblocks `.timeout`, `.limit`, `.dbconfig`, `.vfslist`,
   `.vfsname`, `.show`, `.width`, `.parameter`.

3. **Add a sandboxed file-IO host import.** A narrow
   read/write/append/list trio gated by the same capability
   policy as `.load`. Unblocks `.read`, `.output`, `.once`,
   `.import`, `.dump`, `.backup`, `.restore`, `.save`,
   `.clone`, `.serialize`, `.deserialize`. Not "open every
   file"; specific verbs the extension declares it needs.

The remaining handful (`.exit`/`.quit`, `.trace`, `.auth`,
`.log`) probably stay in the cli forever — they edit per-
process state that exists before any extension is loaded.

## Estimated effort

Rough breakdown:

| Lift                    | Estimate |
|-------------------------|----------|
| Widen world for loader  | 2–3 days (host trait, cli-side bindgen, tests) |
| Expand `spi`            | 3–4 days (each new host call + plumbing + smokes) |
| Sandboxed file-IO       | 4–5 days (capability model, host gate, smokes) |
| Migrate each command    | ~0.5 day/command after the host surface is ready (×~25 commands) |

So full Phase 5 closure is probably another 4–5 weeks of work
beyond this commit. For the docs portion of Phase 5 (what
landed here) the dot.rs surface is now fully documented even
though it's not gone.

## What's already done in Phase 5

- `AUTHORING-DOTCMD-COMPONENTS.md` — author's guide for
  dot-command extension components.
- `tooling/cli-cheatsheet.md` — `.sqlink` subcommand
  reference + state schema added.
- `PLAN-dotcmd-phase5.md` — this file.

## Stand-down note for follow-up agents

If you pick this up: the right next step is lift #1 (widen the
world for `extension-loader`). That unlocks `.sqlink` migrating
to its own extension, which is the canonical
"plan-as-written" target and surfaces every host-surface
shortfall in turn (you'll find out fast what spi needs).
