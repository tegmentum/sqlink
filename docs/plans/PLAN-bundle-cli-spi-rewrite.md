# PLAN — bundle-cli SPI rewrite (task #533)

Drop bundle-cli's typed `sqlite:extension/bundles` import in favor of calling
`dispatch-bridge.bridged-execute-cas` directly. Survey + checkpoint plan.

## 533.1 — Survey output

### Who imports `sqlite:extension/bundles`?

WIT-layer importers (every world in the sqlite-extension package):
- `sqlite-wasm/wit/deps/sqlite-extension/world.wit` — 14 worlds all declare
  `import bundles;` (minimal, minimal-http, minimal-dns, stateful,
  lifecycle-aware, resolving, collating, authorizing, hooked, wal-aware,
  hookprobe, tabular, bundle-cli, shell-aware).
- Mirrored copies in `extensions/postgis-bridge/wit/deps/...` and
  `examples/rust/runnable-sqlite-demo/wit/deps/...`.
- The world.wit doc-comment (lines 4-16) is explicit: "wal-frames + s3-base
  + build + bundles are imported into every world (not just wal-aware /
  hookprobe / bundle-cli) so the host's per-shape bindgen consistently
  has the bundles::Host traits to satisfy on LoadedState."

Actual guest-side callers (Rust source):
- `extensions/bundle-cli/src/lib.rs:115` —
  `use bindings::sqlite::extension::bundles;`
- **No other extension** under `extensions/*` references `extension::bundles`.

Host-side trait impl:
- `host/src/lib.rs` — `impl bundles::Host for LoadedState` (lines 3995-4228),
  13 typed methods. Every per-shape `bindgen!` invocation lists
  `"sqlite:extension/bundles": super::loaded::sqlite::extension::bundles`
  in its `with:` clause.

Browser polyfill (the `bundlesHandler` block, NOT a separate
`buildBundlesPolyfill` function):
- `browser/src/extension-loader.js` lines 2083-2351 (~268 LoC) — implements
  the typed bundles interface for the composed-cli-worker by delegating
  every method to `bridgedExecuteCas(sql, params)`.
- One caller: `browser/src/composed-cli-worker.js:457`. Bundle-cli path
  only.

### Decision (γ) vs (δ) for host impl

**The plan's path (γ) — "delete the typed bundles::Host impl entirely" —
is blocked at the WIT layer.** Removing the impl while every world still
declares `import bundles;` breaks bindgen across all 14 worlds, all 218
extensions, postgis-bridge, and the runnable-sqlite-demo.

Two viable refinements:

- **γ' (full cleanup):** Remove `import bundles;` from every world in
  every world.wit (sqlink + postgis-bridge + runnable-sqlite-demo); then
  delete `bundles::Host` impl + the `with:`-clause entries in every
  `bindgen!` site. Requires regenerating bindgen for all extensions — a
  much larger change than #533 envisioned.
- **δ (delegate):** Keep `bundles::Host` trait impl satisfied on
  `LoadedState`, but route each method through a freshly-added
  `dispatch-bridge.bridged-execute-cas` native impl. Bundle-cli itself
  imports `dispatch-bridge` directly (not `bundles`). Other worlds still
  declare the import for bindgen symmetry but never call it. Smaller
  blast radius, parallels the v1.5 Phase B pattern.

**Recommendation: δ** for this task's scope. γ' belongs in a separate
follow-up (call it #541 or similar) once we've validated the dispatch
path.

### Decision (α) vs (β) for WIT path

Plan assumption was that `sqlite:extension/bundles` lives in the
`sqlite-loader-wit` package. **It does not.** The interface is defined in
`sqlite-wasm/wit/deps/sqlite-extension/host-spi.wit` lines 816-915. This
collapses (α) and (β):

- The `sqlite:extension` package already lives in
  `sqlite-wasm/wit/deps/sqlite-extension/`.
- The `sqlink:wasm` package (containing `dispatch-bridge.bridged-execute-cas`)
  lives in `sqlite-wasm/wit/dispatch-bridge.wit`.
- Both packages are siblings inside `sqlite-wasm/wit/`. A bundle-cli
  world can `import` from both without restructuring deps anywhere.

**Recommendation: (β-equivalent).** Give bundle-cli a bespoke world that
declares the dispatch-bridge import alongside whatever subset of
sqlite-extension imports it still needs (note: bundle-cli currently uses
the `bundle-cli` world from sqlite-extension/world.wit lines ~250). The
simplest path is to edit that `world bundle-cli { ... }` block in-place:
drop `import bundles;`, add `import sqlink:wasm/dispatch-bridge;`.

### Decision on sub-item (e) schema bootstrap sync

**Needed** — JS-side mirror is real.
- Rust source: `sqlite-cas-cache/src/schema.rs` exposes `SCHEMA_VERSION`,
  `BOOTSTRAP_SCHEMA`, `INSTALL_SCHEMA`, and `MIGRATE_V*_TO_V*` consts.
- JS mirror: `browser/src/extension-loader.js` lines 1927-1995 declares
  `CAS_SCHEMA_VERSION`, `CAS_BOOTSTRAP_SQL`, and
  `CAS_INSTALL_SCHEMA_STATEMENTS` with a comment "transcribed from
  sqlite-cas-cache/src/schema.rs ... MUST stay in sync".
- Bootstrap is invoked by JS-side `ensureCasSchema()` (line 1998),
  called from every `bundlesHandler` method before any CRUD.

If the browser side's bundles handler goes away (per 533.5), the JS
mirror becomes dead. So (e) auto-resolves to "delete the JS schema mirror
along with the polyfill" — only the Rust source remains, owned by
sqlite-cas-cache.

## Blockers identified for user decision

1. **The plan's "native host already implements bridged-execute-cas for
   the browser path" is incorrect.** The native host's `bundles::Host`
   currently calls `sqlite_cas_cache::bundles_exec::*` free functions
   directly (via `Cache::with_bundles_conn`). The browser-side
   bridged-execute-cas implementation lives in
   `sqlite-wasm/sqlite-lib/src/lib.rs` (lines 2114-2138) inside the
   composed wasm binary, NOT in the host crate. Native bundle-cli today
   does not flow through `dispatch-bridge` at all.

   To complete 533, the native host needs a fresh
   `impl bridged_execute_cas` (against `Cache::with_bundles_conn`)
   wired into every per-shape `bindgen!` site. That's net-new work the
   task plan didn't budget for.

2. **Path γ requires removing `import bundles;` from every world.wit**
   (sqlink + postgis-bridge + runnable-sqlite-demo) and regenerating
   bindgen for 218 extensions. Doable, but enlarges the change set
   substantially. Path δ avoids this by keeping the WIT import + a
   thin delegating impl.

3. **Whether (γ') or (δ) is preferred depends on whether the user wants
   the "host bundles::Host impl removed" line item of #487 to be
   discharged in this task or deferred to a follow-up.**

## Checkpoint plan (post-survey, contingent on user direction)

- 533.2: edit `world bundle-cli { ... }` in
  `sqlite-wasm/wit/deps/sqlite-extension/world.wit` — drop `import bundles;`,
  add `import sqlink:wasm/dispatch-bridge;`. Regenerate bundle-cli
  bindgen. Push.
- 533.3: rewire `extensions/bundle-cli/src/lib.rs` handlers (save, build,
  list, show, delete, alias) to call `bridged_execute_cas(sql, params)`
  using `sqlite_cas_cache::bundles_exec::*_SQL` consts vendored as
  string literals into bundle-cli. Parse `query-result` rows into the
  existing user-facing shapes. `cargo build` clean. Push.
- 533.4: per chosen (γ') or (δ):
  - (δ) Convert `impl bundles::Host` in `host/src/lib.rs` to delegate to
    a new `impl bridged_execute_cas` against `Cache::with_bundles_conn`.
    Wire bridged-execute-cas into every per-shape bindgen `with:`
    clause.
  - (γ') Remove `import bundles;` from every world in every world.wit;
    drop `bundles::Host` impl + `with:` clause; regenerate bindgen for
    all extensions. sqlink-host tests pass (>=64/64). Push.
- 533.5: delete `bundlesHandler` block (browser/src/extension-loader.js
  2083-2351) and the `CAS_BOOTSTRAP_SQL` / `CAS_INSTALL_SCHEMA_STATEMENTS`
  consts (lines 1927-1995). composed-bundle.spec.js +
  composed-prefix.spec.js still pass. Push.
- 533.6: (e) auto-resolved by 533.5; document the resolution in this
  plan. Push.
- 533.7: append "Done" section with LoC delta + verification results.
  Push.

## Verification gates

- `cargo test -p sqlink-host --lib` >= 64/64 (Phase B baseline) plus
  any new tests for the dispatch path.
- `cd extensions/bundle-cli && cargo test` — passes.
- `cd browser && npm test -- --grep 'composed-(bundle|prefix)'` — both
  pass including non-empty bundle reload leg.
- Native `.bundle save myset --no-build` against `~/.cache/sqlink/cas.db`
  works end-to-end with same output as before.
- Native `.bundle list` returns the saved bundle.
