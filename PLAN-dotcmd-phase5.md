# Phase 5 — dot.rs deletion readiness

Status as of this commit: Phases 1–4 of `PLAN-dotcmd-plugins.md`
have landed; Phase 5 has shipped its docs portion (this file,
`AUTHORING-DOTCMD-COMPONENTS.md`, `tooling/cli-cheatsheet.md`
update). The remaining Phase 5 deliverable — "remove
`cli/src/dot.rs` entirely; everything now flows through the
registry" — is bigger than the plan estimate suggested. This
file is the inventory of what still lives in `dot.rs` and
`lib.rs`'s `eval_input` strip-prefix chain, with the migration
shape for each.

## What's left in the cli's hard-coded dispatch

### `cli/src/dot.rs` (15 arms)

| Command         | Class                        | Migration target |
|-----------------|------------------------------|------------------|
| `.show`         | reads cli-state              | core-dotcmd, needs `cli-state.get-*` read path |
| `.width`        | mutates cli-state            | core-dotcmd, needs `display/width` delta |
| `.timeout`      | direct sqlite3 ffi           | core-dotcmd, needs `spi.busy-timeout` |
| `.parameter`    | mutates cli-state map        | core-dotcmd, needs map-shaped delta (new key class) |
| `.dbconfig`     | direct sqlite3 ffi           | core-dotcmd, needs `spi.db-config-get/set` |
| `.limit`        | direct sqlite3 ffi           | core-dotcmd, needs `spi.limit` |
| `.sha3sum`      | direct sqlite3 ffi + crypto  | own extension (`sha3-sum`) — independent of cli |
| `.sqlink`       | needs extension-loader       | own extension once dotcmd-aware imports loader |
| `.vfslist`      | direct sqlite3 ffi           | core-dotcmd, needs `spi.list-vfs` |
| `.vfsname`      | direct sqlite3 ffi           | core-dotcmd, needs `spi.vfs-name(db)` |
| `.archive`      | sqlite3_archive / zip vtab   | own extension (`archive-cli`) |
| `.session`      | sqlite3_session              | own extension (`session-cli`) |
| `.serialize`    | sqlite3_serialize            | own extension (`serialize-cli`) |
| `.deserialize`  | sqlite3_deserialize          | own extension (`serialize-cli`) |
| `(.show etc.)`  | also rendering current state | tied to above |

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
