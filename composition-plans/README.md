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
  artifact (`cli_with_sqlite.component.wasm`). **Currently
  blocked by substrate gaps; see
  [docs/notes/orchestration-substrate-gaps.md](../docs/notes/orchestration-substrate-gaps.md).**
- `postgis-shim.plan.json` — replaces the `wac plug` recipe in
  `postgis-sqlink-bridge` (Tier 1 A2). Composes
  `postgis-sqlink-bridge` + `postgis-composed` into
  `postgis-sqlink-loadable.wasm`. **Currently blocked by the 100MB
  blob-store limit (postgis-composed is 112MB); see
  substrate-gaps doc.**
- `mobilitydb-shim.plan.json` — replaces the `wac plug` recipe in
  `mobilitydb-sqlink-bridge` (Tier 1 A2). Composes
  `mobilitydb-sqlink-bridge` + `mdb-temporal-wasm` +
  `postgis-composed` into `mobilitydb-sqlink-loadable.wasm`.
  **Blocked by the same blob-store limit.**

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

The right pattern (deferred to Tier 1.1.b enablement; see substrate
gaps) is a build-script step that:

1. Hashes each input component;
2. Writes them into the plan template;
3. Hands the rendered plan to `composectl emit`.

The plan files in this directory currently capture the **shape**
(components, bindings, policy) rather than a specific build's
digests. They are validated against the schema; the digests are
placeholders that the build-script step will fill in.

## See also

- `../docs/plans/PLAN-orchestration-integration.md` — the parent
  plan.
- `../docs/notes/orchestration-dependency.md` — Tier 0 dep model.
- `../docs/notes/orchestration-substrate-gaps.md` — concrete
  substrate gaps that block Tier 1 enablement (parallel
  cross-check stays staged until upstream addresses them).
- `~/git/webassembly-component-orchestration/SPEC.md` — the
  `sys:compose@1.0.0` specification.
