# Composition plans (sys:compose)

Tier 1 of [`PLAN-orchestration-integration.md`](../docs/plans/PLAN-orchestration-integration.md).

These are the `sys:compose@1.0.0` declarative plans that replace the
`composition-*.wac` recipes consumed by `wac compose` / `wac plug`
today. Each plan describes an explicit component graph
(components + bindings + policy) that `composectl emit` can compose
into a sealed wasm artifact, mirroring what `wac` does, plus a
verifiable plan digest.

## Files

- `sqlink-runtime.plan.json` — replaces
  `composition-cli-sqlite-lib.wac` (Tier 1 A1). Composes
  `sqlite-cli` + `sqlite-lib` into the runnable composed runtime
  artifact (`cli_with_sqlite.component.wasm`). Carries
  `explicit_exports` for `sqlite:extension/types@1.0.0` +
  `sqlink:wasm/dispatch-bridge@0.1.0` (the wac recipe's
  load-bearing non-root re-exports).
- `postgis-shim.plan.json` — replaces the `wac plug` recipe in
  `postgis-sqlink-bridge` (Tier 1 A2). Composes
  `postgis-sqlink-bridge` + `postgis-composed` into
  `postgis-sqlink-loadable.wasm`.
- `mobilitydb-shim.plan.json` — replaces the `wac plug` recipe in
  `mobilitydb-sqlink-bridge` (Tier 1 A2). Composes
  `mobilitydb-sqlink-bridge` + `mdb-temporal-wasm` into
  `mobilitydb-sqlink-loadable.wasm`.

Upstream substrate gaps that previously blocked emit-side
cutover are all closed (see
[docs/notes/orchestration-substrate-gaps.md](../docs/notes/orchestration-substrate-gaps.md)).
The build scripts now honour `SQLINK_COMPOSE_TOOL=composectl|both|wac`
to select the emitter; `both` runs the parallel cross-check.

## Lifecycle (parallel cross-check pattern, per
[PLAN-orchestration-integration.md](../docs/plans/PLAN-orchestration-integration.md))

1. Build all input components.
2. Compute their sha256 digests.
3. Render the plan template with current digests.
4. Run `composectl plan validate` → confirms structural validity.
5. Run `composectl emit build plan.json -o composectl-out.wasm`.
6. Run `wac compose` / `wac plug` → produce `wac-out.wasm`.
7. Compare WIT surface of both: imports, exports, instances-of
   each interface, world-shape.
8. After one release of soak with both wac and composectl agreeing,
   retire wac.

The composectl-emitted artifact additionally yields a `plan digest`
+ `emit digest` pair that the wac path can't produce. These get
shipped alongside the wasm artifact for downstream verifiability
(Tier 3 territory).

## Digest discipline

The components' digests in each plan are byte arrays of the
component file's SHA-256. They change every time the input
components are rebuilt — i.e. EVERY release.

The plan files in this directory capture the **shape** (components,
bindings, policy) with placeholder digest bytes
(`[171, 205, 239, 0, 1, 2, …]`). The real digests are filled in at
build time by [`scripts/render-shim-plan.sh`](../scripts/render-shim-plan.sh)
(Tier 1.1.b), which:

1. Hashes each input `.wasm` component (peer-repo build artifacts
   for the postgis / mobilitydb shims, e.g.
   `~/git/postgis-wasm/postgis-composed.wasm`);
2. Substitutes the SHA-256 into `components[i].digest` in the plan;
3. Writes the rendered plan to `composition-plans/build/{shim}-shim.rendered.plan.json`;
4. Optionally (`--emit` / `SQLINK_COMPOSECTL_EMIT=1`) hands the
   rendered plan to `composectl emit build`, staging the input
   blobs into the local CAS beforehand.

Usage:

```sh
scripts/render-shim-plan.sh postgis        # writes composition-plans/build/postgis-shim.rendered.plan.json
scripts/render-shim-plan.sh mobilitydb --emit  # renders + calls composectl emit build
```

`sqlink-runtime.plan.json` is rendered inline by
`scripts/build-composed-runtime.sh` (`emit_via_composectl()`)
because its inputs are built in the same script; the shim plans
render via `render-shim-plan.sh` because their inputs come from
peer repos.

The rendered plans live in a gitignored `build/` subdirectory so
the templates in `composition-plans/*.plan.json` stay stable
across builds. Only the placeholder-carrying templates are
committed.

## See also

- `../docs/plans/PLAN-orchestration-integration.md` — the parent
  plan.
- `../docs/notes/orchestration-dependency.md` — Tier 0 dep model.
- `../docs/notes/orchestration-substrate-gaps.md` — concrete
  substrate gaps that block Tier 1 enablement (parallel
  cross-check stays staged until upstream addresses them).
- `~/git/webassembly-component-orchestration/SPEC.md` — the
  `sys:compose@1.0.0` specification.
