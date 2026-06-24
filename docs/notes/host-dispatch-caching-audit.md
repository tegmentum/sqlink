# Audit: dispatch-store caching surface in host/src/lib.rs

Snapshot of the per-extension cached-store landscape as of commit
4e1c07d, taken before extending the pattern to cover hook dispatch
(blocks #441 and unblocks #423 wal-archive).

## What's already cached

`LoadedExtension` already holds the following per-world cached
(Store, Instance) handles, each `Arc<tokio::sync::Mutex<Option<...>>>`:

| Field                       | World                | Helper                      | Used by                                  |
| --------------------------- | -------------------- | --------------------------- | ---------------------------------------- |
| `cached_minimal`            | `minimal`            | `minimal_locked`            | `dispatch_scalar` (default route)        |
| `cached_minimal_http`       | `minimal-http`       | `minimal_http_locked`       | `dispatch_scalar` if `policy.http`       |
| `cached_minimal_dns`        | `minimal-dns`        | `minimal_dns_locked`        | `dispatch_scalar` if `policy.dns`        |
| `cached_stateful`           | `stateful`           | `stateful_locked`           | `dispatch_aggregate_*` + scalar route    |
| `cached_tabular`            | `tabular`            | `tabular_locked`            | `dispatch_vtab_*` (read-only)            |
| `cached_tabular_mutating`   | `tabular-mutating`   | `tabular_mutating_locked`   | `dispatch_vtab_*` if any mutable vtab    |
| `cached_dotcmd_aware`       | `dotcmd-aware`       | (`dispatch_dot_command`)    | dot-command dispatch                     |

Lifecycle:

- Lazy init on the first dispatch into a given world for that extension.
- Owned across the extension's lifetime; eviction is implicit on
  `unload` (drop of the `Arc<LoadedExtension>` drops the cache).
- `refresh_call_budget` is called after each lazy-init to reset
  fuel / epoch deadline per call without rebuilding the store.

## What's NOT cached today (the gap #441 closes)

Every hook dispatcher builds a fresh `Store<LoadedState>` and
re-instantiates the loaded component on every call:

| Dispatcher                | Line  | World instantiated         |
| ------------------------- | ----- | -------------------------- |
| `dispatch_authorize`      | 7722  | `loaded_authorizing::Authorizing` |
| `dispatch_on_update`      | 7764  | `loaded_hooked::Hooked`    |
| `dispatch_on_commit`      | 7801  | `loaded_hooked::Hooked`    |
| `dispatch_on_rollback`    | 7824  | `loaded_hooked::Hooked`    |
| `dispatch_on_wal_hook`    | 7851  | `loaded_hooked::Hooked`    |

Each call path is:

    let linker = make_loaded_hooked_linker(&self.engine)?;
    let mut store = build_loaded_store(&self.engine, &ext, db_path)?;
    let instance = Hooked::instantiate_async(&mut store, &ext.component, &linker).await?;
    instance.<hook-export>().<call>(&mut store, ...).await

That re-instantiation drops everything in the wasm linear memory
between hook firings, so guest-side state (`thread_local!`,
`static AtomicU64`, `OnceLock<Mutex<T>>`) cannot survive even a
single connection's hook flow. Worse: state set in a SCALAR call
on `cached_minimal` isn't visible to the next hook firing, because
the hook builds a different Store.

## Worlds that need caching, per #441 scope

- `hooked` -> add `cached_hooked` + `hooked_locked` helper.
- `wal-aware` -> the world has identical export shape to `hooked`;
  the host binds against `loaded_hooked::Hooked` for both, so the
  same `cached_hooked` covers it. (No separate bindgen needed; the
  guest just authors against a wider world that adds optional
  imports.)
- `authorizing` -> add `cached_authorizing` + `authorizing_locked`
  helper. Needed for symmetry; otherwise an extension whose
  authorize callback wants to read state written by scalar / hook
  paths still sees a clean Store on every dispatch.

## Cross-world coherence (Task 4)

`dispatch_scalar` already routes the call to the most capable
*existing* world. To make hook callbacks see state written by
scalar calls (the wal-archive requirement) we extend that routing
so that, for an extension declaring any hook (`has_authorizer`
|| `has_update_hook` || `has_commit_hook` || `has_wal_hook`):

  * scalar calls route to `cached_hooked` (still calling the
    `scalar-function.call` export, which `hooked` also exposes),
  * authorizer routes to `cached_authorizing` for now (the
    `authorizing` world does not export hooks),
  * hook callbacks route to `cached_hooked`.

A future refinement could collapse authorizer into `hooked` too
by widening the world to include `authorizer` export (matching the
test-bench `hookprobe` world) — out of scope for #441.

The widest-world rule is bounded by manifest compatibility: a
`tabular` extension does not export `wal-hook`, so it cannot be
instantiated as `Hooked`. The current world set keeps the
tabular / hook surfaces disjoint, so we never need to merge those
into one Store.

## Eviction policy

Same as the existing pattern: cache holds for the extension's
lifetime; drop happens implicitly on `unload`. No LRU. No per-
connection scoping. The wal-archive `start({opts})` -> wal-hook
firings sequence relies on this: the scalar `wal_archive_start`
call populates a `OnceLock<Mutex<RingBuffer>>` in the wasm guest;
the subsequent wal-hook dispatch sees it because they share the
cached store for the extension's whole load.
