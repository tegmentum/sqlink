# Orchestration dependency model

Tier 0.1 of [`PLAN-orchestration-integration.md`](../plans/PLAN-orchestration-integration.md).

## Decision

**Workspace path dep — variant: sibling checkout, no Cargo workspace
member.** sqlink consumes `composectl` as a binary tool (built once
from a sibling repo at a well-known path) and does *not* link
`compose-core` into any sqlink crate yet.

The orchestration repo lives at
`$HOME/git/webassembly-component-orchestration` next to sqlink, with
its own `[workspace]`. sqlink scripts find it by a stable env-var
override:

```
SQLINK_ORCH_ROOT=${SQLINK_ORCH_ROOT:-$REPO_ROOT/../webassembly-component-orchestration}
COMPOSECTL_BIN=${COMPOSECTL_BIN:-$ORCH_ROOT/target/release/composectl}
```

Both the smoke test (`scripts/smoke-test-composectl.sh`) and the
Tier 1 build scripts (`scripts/build-composed-runtime*.sh` once they
gain a `composectl emit` step) honor those env vars.

## Why this variant of option (1)

The plan considers four dep-model options. We chose the lightest
variant of (1):

1. **Workspace path dep, this variant (chosen).** Sibling checkout;
   no Cargo workspace coupling. `composectl` is a CLI tool, not a
   library. There's no current Rust-side need to link `compose-core`
   into a sqlink crate. The smoke test is a shell-out, mirroring how
   `build-composed-runtime.sh` consumes `wac` and `wasm-tools`.

2. **Workspace path dep, full Cargo coupling.** `compose-core` and
   `composectl` as `path = "../webassembly-component-orchestration/..."`
   entries in sqlink's `Cargo.toml`. Premature: sqlink doesn't need
   the lib API yet. Adopting full coupling now would force every
   sqlink CI job to compile compose-core's wasmtime stack
   (~3.5 min release build observed) even for changes that don't
   touch composition. We move to this variant when sqlink gains code
   that needs `compose_core::Plan` types or the `EmitHandler` API
   directly (e.g. Tier 2 B1 wit-shape hashing; Tier 3 C1 in-process
   plan digesting).

3. **Vendor as submodule.** Avoids the side-by-side checkout
   requirement but adds a submodule pointer commit dance to every
   upstream bump. Defer until either CI starts demanding hermetic
   builds or sqlink ships a pinned orchestration release.

4. **Published crate.** Right answer once both projects ship 1.0 and
   the orchestration repo commits to a semver cadence. Premature
   today.

5. **Standalone binary (sibling Cargo install).** Equivalent to the
   chosen variant in shape but loses the per-machine version-pin
   discipline of building from the sibling source. The sibling
   checkout *is* the version pin.

## Operational notes

- `scripts/smoke-test-composectl.sh` auto-builds composectl if it
  isn't already built. First build is ~3.5 min; incremental rebuilds
  are seconds.
- The orchestration repo is not (yet) in the tegmentum org. We
  reference it by sibling path; we do NOT vendor or fork it.
- When CI runs Tier 1 build scripts, both `wac` (existing) and
  `composectl` (new) must be available. The CI image bootstrap step
  builds composectl from the sibling checkout at the same revision
  pin sqlink expects (TBD: ship a `tools/orchestration-rev.txt` once
  the upstream stabilises).

## When this decision changes

- **Move to variant 2 (full Cargo path-dep)** when:
  - Tier 2 lands and sqlink's `verify-catalog` or contract-guard
    needs to call `canon_wit::shape_id` directly.
  - Tier 3 C1 lands and a sqlink crate needs `Plan` serialization
    for plan-digest emission in build metadata.

- **Move to variant 3 (submodule)** when:
  - CI hermeticity (no sibling clones) becomes a hard requirement.
  - sqlink starts shipping a pinned orchestration revision to
    downstream consumers.

- **Move to variant 4 (crates.io)** when:
  - Orchestration repo cuts a 1.0 release with a stable WIT package
    surface and published `sys-compose` / `composectl` crates.
