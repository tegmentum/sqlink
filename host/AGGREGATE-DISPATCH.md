# Aggregate + Collation Dispatch — Status

## TL;DR

The cross-component dispatch surface for aggregate functions and
custom collations is fully wired *structurally*: WIT methods exist,
C trampolines exist, registration logic in
`wasm_register_dynamic_manifest` walks `manifest.aggregate_functions`
and `manifest.collations`, and the host exposes
`Host::dispatch_aggregate_step` / `dispatch_aggregate_finalize` /
`dispatch_collation`. Those host methods currently return
"not implemented yet" because the **loaded-extension bindgen is the
`minimal` world**, which exports neither `aggregate-function` nor
`collation`. Wiring real dispatch requires bringing in a wider world.

## The pipeline today

```
SQL: SELECT my_sum(x) FROM t;
 → SQLite engine
 → wasm_dyn_xstep (extension-unified.c)        ✓ implemented
 → sqlite_wasm_dispatch_aggregate_step          ✓ generated
 → HostWrap::aggregate_step                     ✓ implemented
 → Host::dispatch_aggregate_step                ✗ stub returns Ok(Err(...))
   ↳ would: instantiate loaded ext as Stateful,
            call sqlite_extension_aggregate_function().call_step(...)
```

Same shape for `aggregate_finalize` and for `collation_compare`.

## What's needed to make it real

Single self-contained change:

1. Add a second bindgen alongside `loaded::Minimal`:
   ```rust
   pub mod loaded_stateful {
       wasmtime::component::bindgen!({
           path: "../sqlite-loader-wit/wit",
           world: "stateful",
           with: {
               "sqlite:extension/types": super::loaded::sqlite::extension::types,
               "sqlite:extension/spi": super::loaded::sqlite::extension::spi,
               "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
               "sqlite:extension/config": super::loaded::sqlite::extension::config,
               "sqlite:extension/policy": super::loaded::sqlite::extension::policy,
           },
       });
   }
   ```
   The `stateful` world adds `state` and `cache` imports plus the
   `aggregate-function` export. The `with:` clause shares the
   already-generated types module so we don't pay the duplicate-type
   compilation cost.

2. Add `state::Host` and `cache::Host` impls on `LoadedState` (small
   — five methods each, in-memory `HashMap<String, SqlValue>` per
   loaded extension is fine for v1).

3. Add a `make_loaded_stateful_linker` that registers WASI + state +
   cache + every interface `minimal` already wires.

4. Replace the `dispatch_aggregate_step` / `dispatch_aggregate_finalize`
   bodies:
   ```rust
   let linker = make_loaded_stateful_linker(&self.engine)?;
   let mut store = build_loaded_store(&self.engine, ext)?;
   let instance = loaded_stateful::Stateful::instantiate(
       &mut store, &ext.component, &linker
   ).map_err(|e| anyhow!(...))?;
   let result = instance
       .sqlite_extension_aggregate_function()
       .call_step(&mut store, func_id, context_id, &args)
       .map_err(|e| anyhow!(...))?;
   ```

5. For collation, repeat with a `loaded_full` bindgen (the `full`
   world is the one that exports `collation`).

6. Track `aggregate_functions` and `collations` on `LoadedExtension`
   so `stub_manifest` (now: `aggregate_functions: vec![]`) populates
   them from the read manifest.

## Validation plan

Once 1–6 are in:

```rust
// agg-extension/src/lib.rs — built against the stateful world
struct State { sum: i64 }
impl AggregateFunction for Guest {
    fn step(_id, ctx_id, args) {
        // use context_id-keyed map for `sum`
    }
    fn finalize(_id, ctx_id) -> Result<SqlValue, String> {
        // return SqlValue::Integer(sum)
    }
}
```

```
sqlite> .load agg-extension.wasm
Loaded extension: agg-extension 0.1.0 (1 aggregate function)
sqlite> SELECT wasm_sum(x) FROM (VALUES(1),(2),(3));
6
```

That round-trip is the acceptance criterion for the follow-up.

## Why this is parked

The structural code lands cleanly and isolates the remaining work
to one file (`host/src/lib.rs`) plus one new extension crate. The
pieces are clean to assemble but each individual step (a second
bindgen, two new Host trait impls, dispatching through a different
world type, building a Rust extension against `stateful`, threading
context-keyed state through the host) is real engineering with its
own debugging surface. Better to land the dispatch wiring as a
single focused PR than to mix it into the structural work that's
already landed.

## Related

- `wit/dispatch.wit` — the dispatch WIT methods already include the
  aggregate and collation entries.
- `src/exports/extension-unified.c` — `wasm_dyn_xstep`,
  `wasm_dyn_xfinal`, `wasm_dyn_xcompare`, registration logic.
- `host/src/lib.rs` — `Host::dispatch_aggregate_step` (line ~456),
  `dispatch_aggregate_finalize`, `dispatch_collation`, all currently
  stubbed.
- `host/SPI.md` — companion document for the in-WASM SPI stub
  problem, which has the same "structurally wired, semantically
  stubbed" shape.
