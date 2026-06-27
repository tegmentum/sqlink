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

## Coordinator decisions (received 2026-06-27)

- Path δ confirmed (keep `bundles::Host` impl as a delegate to a new
  native `bridged_execute_cas`).
- Net-new native dispatch-bridge work approved, in scope for #533.

## 533.1.5 — Third blocker discovered during checkpoint setup

After execution began on 533.2, three additional structural facts
surfaced that materially change the plan's checkpoint structure.

### No `world bundle-cli` exists in any world.wit

- `sqlite-loader-wit/wit/world.wit` declares 15 worlds: `minimal`,
  `minimal-http`, `minimal-dns`, `stateful`, `lifecycle-aware`,
  `resolving`, `collating`, `authorizing`, `hooked`, `wal-aware`,
  `hookprobe`, `tabular`, `tabular-mutating`, `dotcmd-aware`, `full`.
  **No `world bundle-cli`.**
- The vendored mirror `sqlite-wasm/wit/deps/sqlite-extension/world.wit`
  has the same 15 worlds (only `package` version differs:
  `sqlite:extension@1.0.0` upstream vs `@0.1.0` vendored).
- bundle-cli's `wit_bindgen::generate!` block targets
  `path: "../../sqlite-loader-wit/wit"` with `world: "dotcmd-aware"`
  (`extensions/bundle-cli/src/lib.rs:99-105`). It shares
  `dotcmd-aware` with every other dot-command extension.

So 533.4 as the coordinator stated ("edit the `world bundle-cli` block
in `sqlite-wasm/wit/deps/sqlite-extension/world.wit`'s") cannot be
executed verbatim. Three viable substitutes:

1. **Add `import sqlink:wasm/dispatch-bridge;` to `world dotcmd-aware`.**
   Affects every dot-command extension (bundle-cli, sqlink-meta-cli,
   etc.). Each one gains a host capability they don't use. Net
   negative for surface hygiene.
2. **Introduce a new `world dotcmd-aware-cas`** that extends
   dotcmd-aware's import set with dispatch-bridge, and switch
   bundle-cli's bindgen to it. New world block in
   sqlite-loader-wit + new bindgen module in `host/src/lib.rs`.
3. **Add a new `world bundle-cli`** to sqlite-loader-wit/wit/world.wit
   pinned to bundle-cli's actual import set (types, spi, session,
   logging, config, cli-stdout, cli-stderr, cli-state, build,
   bundles, loader-bridge + the new dispatch-bridge). Most surgical;
   mirrors the v1.5 "purpose-built world per extension class"
   pattern.

**Recommendation: option 3.**

### Dispatch-bridge has 13 methods, not 1

`sqlite-wasm/wit/dispatch-bridge.wit:34-269` defines 13 methods:
`bridged-execute`, `bridged-execute-cas`, `register-host-scalar`,
`register-host-aggregate`, `register-host-collation`,
`register-host-authorizer`, `register-host-update-hook`,
`register-host-commit-hook`, `register-host-rollback-hook`,
`register-host-wal-hook`, `register-host-vtab`,
`unregister-extension`. A native `impl dispatch_bridge::Host` must
implement all 13. The `register-host-*` methods install
`sqlite3_create_function_v2` trampolines on sqlite-lib's internal
in-wasm connection — meaningful only inside the composed binary,
not on the native host (which has no equivalent
"loaded-extension-visible" connection at present).

Three options:

1. Stub the `register-host-*` methods on the native host with
   `SQLITE_ERROR("not supported on native")`. Honest, fails closed.
2. Split the WIT interface: factor `bridged-execute` and
   `bridged-execute-cas` into a new `interface dispatch-bridge-cas`
   in the same `sqlink:wasm` package; the native host implements
   only that. The composed binary's existing dispatch-bridge keeps
   all 13.
3. Land the full dispatch-bridge native impl, including building a
   host-side connection that loaded extensions can install trampolines
   on. Substantial new wiring beyond #533 scope.

**Recommendation: option 2.**

### Bindgen wiring surface (pre-survey misread)

The original survey reported 16 `bindgen!` sites in
`host/src/lib.rs`. Per the option-3 + option-2 combination above,
only ONE new bindgen module is needed (`loaded_bundle_cli`); the
existing 16 don't need modification because they don't import the
new `dispatch-bridge-cas` interface.

## Revised execution plan (supersedes coordinator's literal 533.2-533.7)

- **533.2 (revised)**: WIT additions
  - `sqlite-wasm/wit/dispatch-bridge.wit` — add
    `interface dispatch-bridge-cas { bridged-execute-cas: func(sql:
    string, params: list<sql-value>) -> result<query-result,
    sqlite-error>; }` (move/copy from `interface dispatch-bridge`).
  - `sqlite-loader-wit/wit/deps/` directory created with
    `sqlink-wasm.wit` containing the dispatch-bridge-cas interface
    summary, OR a symlink/vendored copy of the upstream
    dispatch-bridge.wit, OR a `wit/deps.toml` entry pointing at
    the sqlite-wasm package.
  - `sqlite-loader-wit/wit/world.wit` — add `world bundle-cli`
    block: trimmed import set (types, spi, session, logging,
    config, cli-stdout, cli-stderr, cli-state, build, bundles,
    loader-bridge) + `import sqlink:wasm/dispatch-bridge-cas`.
  - Vendored mirror `sqlite-wasm/wit/deps/sqlite-extension/world.wit`
    gets the same new world block.
  - Separate commit + push on sqlite-loader-wit submodule (HTTPS).
  - Push sqlink with bumped submodule pointer.

- **533.3 (revised)**: Native host impl
  - New `pub mod loaded_bundle_cli` bindgen in `host/src/lib.rs`
    targeting `world: "bundle-cli"`. Shares
    `loaded::sqlite::extension::*` via `with:` clause.
  - `impl loaded_bundle_cli::sqlink::wasm::dispatch_bridge_cas::Host
    for LoadedState` with the single `bridged_execute_cas` method:
    gate on `bundles_granted`, open cache, run SQL via
    `cache.with_bundles_conn(|conn| { prepare/bind/collect })`.
  - sqlink-host builds clean. `cargo test -p sqlink-host --lib`
    still >= 64/64. Push.

- **533.4 (revised)**: bundle-cli rewire
  - `extensions/bundle-cli/src/lib.rs:101` — change world to
    `"bundle-cli"`.
  - `extensions/bundle-cli/src/sql.rs` (new) — vendored copies of
    `sqlite_cas_cache::bundles_exec::*_SQL` consts.
  - Replace each typed `bundles::*` call (the 12 in
    `extensions/bundle-cli/src/lib.rs`) with
    `dispatch_bridge_cas::bridged_execute_cas(sql, params)` plus
    row parsing.
  - Multi-statement methods (e.g. `bundle_save`) become explicit
    transactions: BEGIN -> INSERT bundle -> INSERT members -> COMMIT,
    each as a separate `bridged_execute_cas` call.
  - `cargo build -p bundle-cli-extension` clean. Push.

- **533.5 (NEW)**: Path δ refactor of `bundles::Host`
  - The original 533.4 lives here. Refactor
    `host/src/lib.rs:4009-4287`'s `impl bundles::Host` into thin
    delegates that internally call a shared
    `cas_exec(cache, sql, params)` helper used by both the typed
    methods AND the new `bridged_execute_cas` impl. Net effect:
    single SQL surface inside the host.
  - `cargo test -p sqlink-host --lib`: >= 64/64. Push.

- **533.6**: Browser polyfill shrinkage
  - Delete `bundlesHandler` (`browser/src/extension-loader.js:2083-2351`)
    and the CAS schema mirror (`:1927-1995`) — bundle-cli no longer
    imports the typed bundles WIT, so the polyfill is dead.
  - composed-bundle.spec.js + composed-prefix.spec.js still pass.
  - Push.

- **533.7**: Plan doc closeout — append `## #533 — Done` with LoC
  deltas, paths actually taken, verification results. Push.

### Footprint estimate

- `sqlite-wasm/wit/dispatch-bridge.wit`: ~10 LoC added
- `sqlite-loader-wit/wit/world.wit`: ~25 LoC (new world) +
  ~3 LoC (deps reference)
- `sqlite-loader-wit/wit/deps/sqlink-wasm.wit` (new): ~10 LoC
- `host/src/lib.rs`: ~25 LoC (bindgen module) + ~40 LoC (trait impl)
  + ~120 LoC (bundles::Host delegate refactor, net -50)
- `extensions/bundle-cli/src/lib.rs`: net ~+150 LoC (SQL strings
  + row parsing + transactions - typed call sites)
- `extensions/bundle-cli/src/sql.rs` (new): ~120 LoC
- `browser/src/extension-loader.js`: net -293 LoC

Roughly +200 LoC Rust, -293 LoC JS, +50 LoC WIT.

## Stop point and next-step decision request

I'm pausing here because the revised plan adds two changes the
coordinator's message didn't approve:

1. **Interface split**: moving `bridged-execute-cas` from
   `interface dispatch-bridge` into a new `interface
   dispatch-bridge-cas` (or accepting the 12 stubs on the native
   host). The split needs a sign-off from whoever owns the
   composed-binary contract because the existing browser code
   calls `b.bridgedExecuteCas(...)` — moving the method to a new
   interface changes the generated JS binding name path.
2. **New `world bundle-cli`** in `sqlite-loader-wit`. Adds a
   purpose-built world for a single extension, sets a precedent
   that other near-singleton extensions (sqlink-meta-cli?) may
   follow. Worth confirming before stamping in.

Honest assessment of remaining session budget: the original 533.2-7
list (per coordinator's message) implies 3-5 days of cross-cutting
work even with the revised plan above. Cargo cold rebuilds on this
workspace run 5-15 min each; each WIT change triggers one. There
are at least 6 such cycles in the revised plan (one per
checkpoint). Honest deferral: I cannot complete 533.2-533.7 in this
session.

What I can do in this session if the user confirms the revised plan:

- **533.2 only** (WIT additions + sqlite-loader-wit submodule push
  + sqlink submodule pointer bump). Roughly 200-300 lines of WIT
  + commit + push. No cargo build needed.
- Or **533.5 only** (host-side path δ refactor without bundle-cli or
  WIT changes — just thread `Cache::with_bundles_conn` through a
  shared helper that both typed bundles::Host and a future
  dispatch-bridge-cas impl can use). Roughly 150 lines of Rust.
  Builds + tests. Best value for this session's remaining budget.

## Checkpoint plan (legacy — superseded by Revised execution plan above)

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
