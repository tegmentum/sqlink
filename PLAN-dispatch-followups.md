# Plan: Implement Open Dispatch Follow-Ups

## Overview

Three follow-ups remain after the structural dispatch surface
landed (commits `5678984` + `7f7f863`):

| # | Name | Scope | Cost |
|---|------|-------|------|
| 1 | Aggregate dispatch (Stateful) | wider-world bindgen + state/cache hosts + agg test ext + end-to-end | 2–3 days |
| 2 | Collation dispatch (Full) | second wider-world bindgen + collation test ext | 1 day |
| 3 | Hook dispatch (authorizer + update-hook + commit-hook) | per-world bindgens + per-hook trampolines on the in-WASM side | 2 days |
| 4 | In-WASM SPI (async + cooperative scheduling) | full async wasmtime conversion of host + CLI; out-of-scope for this plan | weeks |

This plan covers Follow-Ups 1–3 in dependency order. Follow-Up 4 is
documented in `host/SPI.md` and tracked separately; it changes the
runtime model of the host (sync → async) and warrants its own
spike + design doc.

## Background

What's already landed:

- `wit/dispatch.wit` declares `aggregate-step`,
  `aggregate-finalize`, `collation-compare`.
- `extension-unified.c` implements the C trampolines
  (`wasm_dyn_xstep`, `wasm_dyn_xfinal`, `wasm_dyn_xcompare`) and
  iterates `manifest.aggregate_functions` + `manifest.collations` at
  registration time.
- `HostWrap` routes the dispatch calls into `Host::dispatch_*`
  methods, all of which return "not implemented yet" today.
- `host/AGGREGATE-DISPATCH.md` documents the six-step path to
  closure for aggregate + collation.

What's not yet wired:

- The `loaded` bindgen is `world: "minimal"` only, which exports
  metadata + scalar-function. To call into an extension's
  `aggregate-function` or `collation` export, the host needs a
  bindgen against a wider world.
- `LoadedExtension` records only `scalar_functions`; aggregate and
  collation manifest entries are dropped on the floor by
  `stub_manifest`.
- No hook-class dispatch WIT methods exist yet.

## Follow-Up 1 — Aggregate Dispatch (Stateful world)

### Goal

End-to-end:

```
sqlite> .load agg-extension.wasm
sqlite> SELECT wasm_sum(x) FROM (VALUES(1),(2),(3));
6
```

### Steps

1. **Second bindgen in `host/src/lib.rs`** — alongside `loaded`:
   ```rust
   pub mod loaded_stateful {
       wasmtime::component::bindgen!({
           path: "../sqlite-loader-wit/wit",
           world: "stateful",
           with: {
               "sqlite:extension/types":   super::loaded::sqlite::extension::types,
               "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
               "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
               "sqlite:extension/config":  super::loaded::sqlite::extension::config,
               "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
           },
       });
   }
   ```
   The `with:` clause shares already-generated types so we avoid
   the duplicate-record compile cost.

2. **`state::Host` + `cache::Host` impls on `LoadedState`** — `state`
   methods (get/set/delete/keys/clear) backed by
   `HashMap<String, SqlValue>` keyed per loaded extension (so two
   extensions don't tread on each other's keys). `cache` is the same
   shape minus TTL enforcement for v1 (cache stores values, TTLs
   stored alongside but not yet expired — adequate for an in-process
   loaded extension whose lifetime is short).

   Store the per-extension state maps on `Host` itself (behind the
   same `RwLock` as `components`) so they survive across dispatch
   calls. `LoadedState` (per-call store state) borrows them in via
   an `Arc`.

3. **`make_loaded_stateful_linker`** — separate constructor next to
   `make_loaded_linker`. Calls `wasmtime_wasi::p2::add_to_linker_sync`
   then `loaded_stateful::Stateful::add_to_linker::<_, LoadedHostData>`.

4. **Real `dispatch_aggregate_step` / `dispatch_aggregate_finalize`**
   in `Host`:
   ```rust
   let linker = make_loaded_stateful_linker(&self.engine)?;
   let mut store = build_loaded_store(&self.engine, ext)?;
   let instance = loaded_stateful::Stateful::instantiate(
       &mut store, &ext.component, &linker,
   ).map_err(|e| anyhow!("instantiate {ext_name} as stateful: {e}"))?;

   let loaded_args: Vec<_> = args.into_iter()
       .map(convert_sql_value_to_loaded).collect();
   let result = instance
       .sqlite_extension_aggregate_function()
       .call_step(&mut store, func_id, context_id, &loaded_args)
       .map_err(|e| anyhow!("call_step: {e}"))?;
   Ok(result)  // already result<_, String>
   ```
   `dispatch_aggregate_finalize` mirrors the shape, calls
   `call_finalize`, converts the returned SqlValue back through
   `convert_sql_value_from_loaded`.

5. **Track aggregate manifest entries** — extend `LoadedExtension`
   with `pub aggregate_functions: Vec<AggregateFunctionEntry>` and
   populate at load-time (`load_extension` reads them from the
   manifest the way it already reads `scalar_functions`).
   `stub_manifest` populates the WIT `aggregate_functions` field
   from those records so the in-WASM CLI receives them and the C
   registration loop kicks in.

6. **`agg-extension` test crate** — new crate under
   `~/git/sqlite-wasm-loader/extensions/agg-extension/` (or a sibling
   to `test_extension`). Built against `world: "stateful"`. Exports:
   - `metadata.describe` → manifest with one aggregate (`wasm_sum`,
     `num_args: 1`, `is_window: false`)
   - `scalar-function.call` → unused; can return an error for any id
   - `aggregate-function.call_step` → adds `args[0].as_int()` to a
     `context_id`-keyed running sum stored in a `RefCell<HashMap>`
   - `aggregate-function.call_finalize` → reads + removes the sum,
     returns `SqlValue::Integer(total)`
   - `aggregate-function.call_value` / `.call_inverse` → return
     errors (window-mode not implemented for v1)

7. **End-to-end test** — extend `host/tests/load.rs` with a test
   that:
   - Loads `agg-extension.wasm`
   - Asserts the manifest reports one aggregate
   - Drives a small SQL session through the CLI binary (or a direct
     `Host::dispatch_aggregate_step/finalize` integration test) and
     checks `SUM(1+2+3) = 6`.

### Risks

- `bindgen!` `with:` clause type-sharing has version-specific
  syntax in wasmtime 45. Verify against `sqlite-wasm-loader`'s
  bindings.rs pattern (which uses two separate bindgens for the
  same WIT, without `with:`, and pays the duplicate-type cost). If
  `with:` doesn't elide types as hoped, fall back to that pattern
  and add converters between the type universes.
- State map ownership: holding the state map on `Host` behind a
  `RwLock` means writes during a dispatch lock the whole map.
  Acceptable for v1; if contention shows up, switch to a per-key
  finer-grained lock.

## Follow-Up 2 — Collation Dispatch (Full world)

### Goal

```
sqlite> .load coll-extension.wasm
sqlite> CREATE TABLE t (s TEXT COLLATE noaccent);
sqlite> INSERT INTO t VALUES ('café'), ('cafe'), ('CAFE');
sqlite> SELECT s FROM t ORDER BY s COLLATE noaccent;
-- all three sort as equal
```

### Steps

1. **Third bindgen** — `loaded_full` (world: "full") next to
   `loaded_stateful`. Same `with:` strategy. Full additionally needs
   `http::Host` + `prepared::Host` + `transaction::Host` + `schema::Host`
   + `random::Host` + `text::Host` + `hashing::Host` + `encoding::Host`
   impls on `LoadedState` — all stubbed identically to the existing
   `http::Host` pattern (return structured errors).

2. **`make_loaded_full_linker`** — full linker; pre-wires every
   import the full world declares.

3. **Real `dispatch_collation`** — instantiate as Full, call
   `instance.sqlite_extension_collation().call_compare(...)`. Return
   value is `i32` (`s32` in WIT), pass it through unchanged.

4. **Track collations on `LoadedExtension`** — same pattern as
   aggregates: store `collations: Vec<CollationEntry>` and surface
   via the manifest the in-WASM CLI reads.

5. **`coll-extension` test crate** — built against `full` world.
   Exports a single `noaccent` collation that strips accents (or
   for v1, just uppercases both inputs and compares). Returns the
   correct ordering from `compare(_id, a, b)`.

6. **End-to-end test** — load + create table + insert + select-order
   sequence above.

### Risks

- The `full` world declares every import. We need stub impls for
  all of them. Existing `http::Host` impl is the template — repeat
  for each. ~80 LOC of stubs.
- Some `full` exports are gated on manifest flags
  (`has_authorizer`, etc.). `Full::instantiate` requires all of
  those exports to exist on the component, so a `coll-extension`
  that doesn't export `authorizer` or `lifecycle` will fail to
  instantiate as Full. Two ways out:
  - Make `coll-extension` ALSO export stubs for every other Full
    interface. Mechanical but tedious.
  - Add a fourth world to `sqlite-loader-wit/wit/world.wit`
    specifically for "minimal + collation" extensions: imports the
    minimal set, exports metadata + scalar-function + collation.
    Cleaner WIT shape, costs another bindgen.

   Recommendation: add the dedicated world. The Full world stays
   reserved for kitchen-sink extensions.

## Follow-Up 3 — Hook Dispatch (authorizer + update-hook + commit-hook)

### Goal

Loaded extension can register hooks that fire on row updates,
commits, rollbacks, and authorization checks. Manifest already
carries `has_authorizer`, `has_update_hook`, `has_commit_hook` —
this is the wiring that makes them functional.

### Steps

1. **Extend `wit/dispatch.wit`** with:
   ```wit
   authorize: func(
       ext-name: string,
       action: auth-action,
       arg1: option<string>, arg2: option<string>,
       database: option<string>, trigger: option<string>,
   ) -> auth-result;

   on-update: func(
       ext-name: string,
       op: update-operation,
       database: string, table: string, rowid: s64,
   );

   on-commit: func(ext-name: string) -> bool;
   on-rollback: func(ext-name: string);
   ```

2. **C-side hook registration** in
   `wasm_register_dynamic_manifest`:
   - If `manifest.has_authorizer`, call `sqlite3_set_authorizer`
     with a `wasm_dyn_xauthorizer` trampoline + a
     per-extension user-data struct.
   - If `manifest.has_update_hook`, call `sqlite3_update_hook` (db-
     global; only one extension at a time can hold the slot in raw
     SQLite, so document the "last loaded wins" semantics or
     multiplex inside the trampoline if multiple loaded extensions
     declare hooks).
   - Same for commit_hook + rollback_hook.

3. **`HostWrap` impls + `Host::dispatch_authorize` /
   `dispatch_on_update` / `dispatch_on_commit` /
   `dispatch_on_rollback`** — same shape as aggregate dispatch but
   targeting the `authorizer`, `update-hook`, `commit-hook` exports
   on the loaded component. Use whichever bindgen world's linker
   has those exports — `authorizer` is in the `authorizing` world;
   `update-hook` / `commit-hook` are in the `hooked` world. Likely
   wants a fourth + fifth bindgen variant, or a single "kitchen-
   sink" loaded world picked at dispatch time.

4. **Unloading** — when `unload` runs, clear any hooks the
   extension registered (else the trampolines fire into a freed
   `LoadedExtension`). Track registered-handle IDs on the
   `LoadedExtension` so `unload` can call `sqlite3_set_authorizer(db, NULL, NULL)`
   etc.

5. **Test extension** — a hook-extension that exports an
   `update-hook` and counts row-write events into its `state` map.
   Test: load → INSERT a few rows → assert the count via
   `SELECT state_get('row_count')`. (Validates both the hook
   dispatch AND that follow-up 1's state impl is correct.)

### Risks

- `sqlite3_update_hook` and friends are db-global, not
  function-scoped. Two extensions wanting update hooks on the same
  connection can't both have it cleanly via raw SQLite. Either
  document "last loaded wins" or build a fan-out: install ONE
  master trampoline, multiplex inside the host.
  Recommendation: ship "last wins" for v1, log a warning when
  loading an update-hook extension while one is already
  registered.

## Validation

After each follow-up, run:

```bash
make                    # full build, no warnings
make cli-demo-test      # composed-demo path still passes
cd host && cargo test --release   # host unit + integration tests
cd ~/git/sqlite-wasm-loader && cargo test --release   # loader still green
```

End-to-end checks per follow-up:

| Follow-up | Manual smoke |
|---|---|
| 1 | `.load agg-extension.wasm; SELECT wasm_sum(x) FROM (VALUES(1),(2),(3));` → 6 |
| 2 | `.load coll-extension.wasm; SELECT 'café' = 'CAFE' COLLATE noaccent;` → 1 |
| 3 | `.load hook-extension.wasm; INSERT INTO t VALUES (1); SELECT state_get('rows');` → 1 |

## Out of Scope

- **In-WASM SPI** (`host/SPI.md`). Substantial — requires async
  wasmtime conversion or reactor-mode CLI. Separate spike.
- **Window-function semantics** (`aggregate-function.call_value` /
  `.call_inverse`). Adds another dispatch direction; defer until
  there's demand.
- **Authorizer return-value mapping nuances** (SQLITE_OK / DENY /
  IGNORE). The structural surface should pass them through;
  document edge cases as they surface.

## Dependency Graph

```
Follow-up 1 (Aggregate)
    ↓ (state::Host + cache::Host land here)
Follow-up 3 (Hooks)         ← reuses state for the hook-extension test
    ↑
Follow-up 2 (Collation)     ← independent; can be done in parallel with 3
```

Reasonable order: 1 → 2 → 3. Each is a self-contained PR.

## Branch Strategy

One branch per follow-up. Don't bundle — each touches the host
bindgen surface and lands its own test extension. PR-shaped chunks
keep review tractable.

Suggested branch names:
- `feat/aggregate-dispatch`
- `feat/collation-dispatch`
- `feat/hook-dispatch`
