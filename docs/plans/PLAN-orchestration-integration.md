# PLAN: Compositional WebAssembly orchestration integration

## Problem

sqlink ships ~239 wasm component extensions and a composed cli+sqlite-lib
runtime. Composition today uses `wac` scripts (`composition-cli-sqlite-lib.wac`,
the shim-side `wac plug` recipes in datafission, ad-hoc bundle-cli build
paths). That works but leaves the composed artifacts as opaque bytes — no
canonical identity beyond a blake3 of the output, no verifiable plan, no
attestation chain, no trust/secrets channel.

`~/git/webassembly-component-orchestration` (`sys:compose@1.0.0`) provides
the missing layer: declarative plans, canonical WIT + CBOR for deterministic
identity, plan/emit/exec/reflect lifecycle, pluggable trust (PKI / SigStore /
custom) and secrets (PKCS#11 / Vault / HSM), optional audit + attestation +
metrics, and content-addressed emit/exec caches.

Since we're taking the dependency, this plan inventories *every* sqlink
surface where it pays off and tiers the rollout so each step ships independent
value.

## Where it fits in sqlink

### A. Composition surfaces (replace wac)

- **A1. sqlink composed runtime.** `composition-cli-sqlite-lib.wac` becomes
  a `composectl plan`. The build script's `wac compose` step becomes
  `composectl emit`. Gains: a reproducible plan digest per release, a
  content-addressed emit cache (`H(plan + digests + "emit:v1")`), and any
  future re-export-required changes (cf. v1.4 lesson — adding
  `sqlite:extension/types@0.1.0` was load-bearing) are caught at plan
  validate, not at wac compose time with the cryptic
  "instance not valid to be used as export" error.
- **A2. postgis / mobilitydb shim composition.** Currently
  `wac plug postgis-wasm + 14 deps` in `~/git/datafission` produces the
  `postgis-composed.wasm` artifact whose blake3 is pinned in
  `postgis-composed-pin.txt`. Migrate to a plan; the pin becomes the plan
  digest plus the emit digest. Same applies to `mdb-temporal-wasm` and any
  future shim. Cross-project leverage: the sibling `ducklink` repo also
  composes these shims (per `~/git/ducklink/docs/postgis-mobilitydb-
  integration.md`) — a shared plan ships once and emits both targets.
- **A3. bundle-cli compose path.** bundle-cli today saves bundles as a set
  of components keyed by name + content-addressed digest. The save/build
  pipeline could use `composectl emit` directly, eliminating the bespoke
  build orchestration in `extensions/bundle-cli`.

### B. Identity surfaces (canonical WIT)

- **B1. Strengthen `PLAN-wit-contract-versioning.md` (#485).** The plan's
  per-component `wit_contract` field is currently the semver string from
  `package sqlink:wasm@X.Y.Z`. Layer the canonical-WIT shape hash
  (`canon:wit` → `iface-id` / `world-id` / `package-id`) alongside the
  semver — semver remains the human-facing major-bump channel; the shape
  hash is the actual structural identity. Drift detection becomes cheaper
  and stronger: `verify-catalog` compares hashes, not just version strings.
- **B2. Extension registry hashing.** `registry/index.json` entries gain
  a canonical-WIT shape hash for each component's imported `sqlink:wasm`
  shape AND its own exported shape. Two components claiming the same
  contract version but disagreeing on shape get flagged immediately.
- **B3. Manifest channel.** `get-info`'s `extension-info` record gains
  an optional `wit_shape_id` field (the canonical-WIT digest of the
  component's imported `sqlink:wasm` slice). The loader pre-check (#485
  Phase 2) compares it against the host's known shape ID and rejects
  with a clean message before instantiation. Belt-and-suspenders on top
  of the semver guard.

### C. Reproducibility + attestation surfaces

- **C1. CI plan digests.** Every release composed runtime (the cli +
  sqlite-lib binary) ships with its plan digest. Anyone can re-emit from
  the plan + component digests and confirm bit-identical output.
- **C2. OCI artifact signing.** sqlink already publishes to OCI registries
  (`oci_artifact` field per registry entry). The orchestration system's
  SigStore integration signs plans + artifacts; the registry entry gains
  a `attestation` field. Foundation for downstream "did Tegmentum publish
  this exact extension?" verification.
- **C3. Conformance vectors as CI.** sqlink maintains a corpus of plan
  vectors (sqlink-runtime, postgis-composed, mobilitydb-composed,
  a bundle-cli example, the dispatch-bridge family) and runs the
  orchestration repo's conformance runner against them in CI. Catches
  semantics regressions in either the orchestration system or our plan
  authoring.

### D. New capability surfaces

- **D1. Trust on bundle-cli.** `.bundle save` could pull from a private
  bundle registry that requires SigStore-verified plans. bundle-cli today
  has no auth/trust model; this is genuine new product surface.
- **D2. Secrets at extension load.** Extensions could declare required
  secrets in their plan (e.g. an API-key extension wants a token).
  `std:secrets` token indirection means the extension never sees the raw
  secret — it gets a tenant-scoped token, host resolves via PKCS#11 / Vault
  /HSM. Lets sqlink ship extensions that talk to authenticated services
  without leaking creds through SQL.
- **D3. Audit + metrics.** Optional `std:audit` worlds give us a built-in
  trail for sensitive ops (bundle install, extension load). `std:metrics`
  collects extension exec timing without bespoke instrumentation.

### E. Bundles surface

- **E1. Bundles ARE plans.** Today a bundle is a manifest + a set of
  components addressed by digest. A plan is a manifest + a set of
  components addressed by digest + their wiring + their policy. Bundles
  become a thin wrapper over plans (the wiring is "no wiring; load each
  independently into the sqlink host"), gaining trust/policy/reproducibility
  for free. Existing bundle artifacts stay readable via a v1 → v2 adapter.

### F. Side cleanups enabled by the migration

- **F1. `wasm-tools component new` step.** v1.5 round 1 found this is no-
  op on wasm32-wasip2 (the target emits components directly). When we drop
  wac, we also drop incidental `wasm-tools` version coupling in build
  scripts.
- **F2. WAL archive header format.** `PLAN-wal-archive.md` (referenced
  elsewhere) defines an archive header. If we adopt canonical CBOR
  (`cborcanon:1`) for the header, archives become structurally hashable
  and the existing `wal-archive` integrity-check tooling gets stronger
  semantics for free.

## Dependency model — open

How sqlink takes the dependency is a real choice with consequences:

1. **Workspace path dep** (`compose-core`, `composectl` as path = "../webassembly-component-orchestration/...").
   Tightest coupling, immediate iteration, but requires the sibling repo
   present in every dev clone + CI checkout. Same model sqlink already has
   for `sqlite-cas-cache`, `sqlink-shim-codegen`, etc.
2. **Vendor as submodule** (orchestration repo as a sqlink submodule like
   sqlite-wasm). Pins to a specific revision; bumps are explicit. Avoids
   side-by-side checkout requirements.
3. **Published crate** (`sys-compose` on crates.io / private registry).
   Cleanest semantically but requires the orchestration repo to commit to
   semver + publish cadence. Premature if either project is still at 0.x
   iteration.
4. **Standalone binary** (`composectl` in PATH or pinned in `tools/`).
   Loose coupling; only the CLI surface area matters. Right model if the
   orchestration system stabilises its plan format and we want sqlink to
   be insulated from internal churn.

Recommendation: start with (1) for the iteration phase, move to (3) once
both projects are at 1.0 and the orchestration repo commits to a public API
surface. (2) is a fallback if side-by-side checkouts become friction.

## Tiered rollout

### Tier 0 — bring the dependency in

- Pick the dependency model (see above; default to workspace path dep).
- Build `composectl` locally; confirm conformance vectors pass on this
  machine.
- Add a smoke test that calls into `compose-core` for a trivial plan
  (validates the linkage).

### Tier 1 — direct wac replacements

- A1: sqlink composed runtime. Author a plan for cli+sqlite-lib;
  `composectl emit` runs in the build script alongside the existing
  `wac compose` for one release as a parallel-cross-check; then wac retires.
- A2: postgis/mobilitydb shim composition. Coordinate with datafission
  (sibling repo). Same parallel-cross-check pattern. Ducklink benefits
  immediately if it picks up the same plan.

### Tier 2 — canonical-WIT identity (folds into #485)

- B1: add canonical-WIT shape hash to `wit_contract` discipline. Update
  `verify-catalog` (or write it if #485 hasn't shipped yet) to use the
  hash.
- B2: registry entries gain `wit_shape_id` for both imports and exports.
- B3: `extension-info.wit_shape_id` field; loader pre-check uses it.

### Tier 3 — reproducibility + attestation in CI

- C1: every release artifact ships with its plan digest in metadata.
- C3: conformance vectors run on every PR.
- C2: SigStore signing — gated on a real key-management story (separate
  decision: who controls the signing key; SigStore Fulcio integration
  vs. cosign with a Vault-backed key).

### Tier 4 — new capabilities

- D1, D2, D3 are net-new product surfaces, not migrations. Each gets its
  own user-facing decision (do we ship private bundles? do we want
  secrets-aware extensions? what's the audit surface?). Scope per item;
  none is required to call the integration "done".

### Tier 5 — bundles as plans

- E1: bundle-cli bundle save/load becomes a plan emit/exec under the
  hood. Old bundles read via v1→v2 adapter. Captures trust/policy/repro
  for bundles without a separate bundles spec.

## Sequencing

Blocked by:
- #484 (v1.5 round 2 — actively editing `composition-cli-sqlite-lib.wac`
  and the build script; would race Tier 1 A1).
- #485 (WIT contract versioning — Tier 2 folds into its Phase 1 + Phase 2
  surface; landing both at once avoids touching the same `verify-catalog`
  and `registry/index.json` twice).

Tier 0 + Tier 1 can start as soon as both #484 and #485 are merged. Tier
2 is part of #485's execution (do it in the same fork). Tier 3 follows
Tier 1 (need real plans before attestation matters). Tier 4 is decoupled
— start any of D1/D2/D3 whenever a user-facing need surfaces. Tier 5
follows everything (largest behavior change).

## Risks

- **Plan format churn.** orchestration repo is at `@1.0.0` for the spec
  but `composectl`'s CLI surface may iterate. Tier 0 commits us to
  upstream-tracking discipline.
- **Build-time cost.** `composectl emit` does canonicalization +
  validation that wac doesn't; for the 239-component case the extra work
  may dominate. Benchmark before mass adoption.
- **Cross-project coordination.** datafission and ducklink need to agree
  on plans (A2) before we get cross-project leverage. Without that
  agreement, Tier 1 A2 is still a win for sqlink but not for the
  ecosystem.
- **Trust/secrets blast radius.** Tier 4 changes the security model. Each
  D-item gets a separate review before shipping; default off, opt-in via
  capability declaration in the plan.

## Verification

- Tier 1: composed runtime artifact has byte-identical (or at least
  semantically-identical: same exports, same imports, same instance-of
  every interface) output from `composectl emit` vs `wac compose`. Smoke
  tests (composed-prefix, composed-bundle, native cli) all pass against
  the new artifact. Plan digest is reproducible across two independent
  emits.
- Tier 2: `verify-catalog` flags a deliberately-mismatched canonical-WIT
  shape; the friendly contract-mismatch message from #485's Phase 2
  loader pre-check fires on shape disagreement, not just semver.
- Tier 3: conformance runner output appears in CI; a deliberately-tampered
  artifact fails signature verification.
- Tier 4/5: per-item user-facing acceptance (separate plans/specs).

## References

- `~/git/webassembly-component-orchestration/README.md`,
  `~/git/webassembly-component-orchestration/SPEC.md`,
  `~/git/webassembly-component-orchestration/COMPOSITION_INTEGRATION.md`.
- `~/git/sqlink/docs/plans/PLAN-wit-contract-versioning.md` (#485 — Tier
  2 overlaps).
- `~/git/sqlink/docs/plans/PLAN-followups.md` (#484 v1.5 in flight; Tier
  1 A1 blocked).
- `~/git/sqlink/docs/postgis-mobilitydb-integration.md` (Tier 1 A2
  context).
- Sibling `~/git/ducklink/docs/postgis-mobilitydb-integration.md` (cross-
  project leverage).
