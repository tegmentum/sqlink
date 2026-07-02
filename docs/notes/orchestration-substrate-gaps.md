# Orchestration substrate gaps (Tier 1 cutover)

Tier 1 of [`PLAN-orchestration-integration.md`](../plans/PLAN-orchestration-integration.md)
calls for `composectl emit` to run alongside `wac compose` /
`wac plug` as a parallel cross-check, then retire wac after a
soak. This document records the substrate gaps that originally
blocked that cross-check on real sqlink inputs and links to the
upstream fixes that closed them.

**Status: all three gaps resolved upstream** — see the fix
references below each gap. The parallel cross-check is now wired
into `scripts/build-composed-runtime.sh` and
`scripts/build-composed-runtime-single-memory.sh` via
`SQLINK_COMPOSE_TOOL={wac,composectl,both}` (default: `wac`).
Setting `both` runs both emitters and diffs their WIT surfaces.

The plan files in `composition-plans/` now carry
`explicit_exports` (Gap 1's schema hook) so they emit the same
outer-world exports as the `wac compose` recipe.

## Gap 1 — `composectl emit` cannot re-export non-root component instances

**Hits A1 (sqlink composed runtime).** **RESOLVED upstream** in
webassembly-component-orchestration `a7a5a809`
(`feat(sys:compose): extend PlanV1 schema with explicit-exports`)
+ `2e3ee85f` (`feat(compose-core/emit): re-export non-root
instances via wac-graph library`). Plans that carry an
`explicit_exports` list take the wac-graph library emit path.

The composed runtime currently exports three instances:

```
export wasi:cli/run@0.2.6;                  // from sqlite-cli (root)
export sqlite:extension/types@0.1.0;        // from sqlite-lib (dependency)
export sqlink:wasm/dispatch-bridge@0.1.0;   // from sqlite-lib (dependency)
```

The latter two are NOT cli exports — they're sqlite-lib exports
that the wac recipe surfaces in the composed outer world so the JS
host's `spi-loader.register-scalar` impl can call into the
dispatch-bridge trampoline. The full rationale lives in
`composition-cli-sqlite-lib.wac`:

> `wac plug` is no longer sufficient: it auto-strips exports the
> outer world doesn't declare, which silently drops sqlite-lib's
> `dispatch-bridge`. Compose-with-recipe lets us explicitly re-
> export the bridge so the JS host's spi-loader.register-scalar
> impl can call into it.

`composectl emit` today uses `wasm-compose`'s `ComponentComposer`,
which has the SAME limitation — its `Config` exposes `dependencies`
and `instantiations`, but no concept of "alias-export this instance
from a non-root component." Empirical test (see "Test artifacts"
below): `composectl emit` produces an artifact that exports only
`wasi:cli/run@0.2.6`.

**Symptoms in the cross-check output:**

```
$ wasm-tools component wit composectl-sqlink-runtime.wasm | grep export
  export wasi:cli/run@0.2.6;

$ wasm-tools component wit wac-sqlink-runtime.wasm | grep export
  export wasi:cli/run@0.2.6;
  export sqlite:extension/types@0.1.0;
  export sqlink:wasm/dispatch-bridge@0.1.0;
```

The composectl artifact would load, but `spi-loader.register-scalar`
would fail to find the dispatch-bridge alias-export, and any host
calling `dispatch-bridge.bridged-execute*` on the composed component
would get an export-not-found at instantiation time.

**Upstream fix shape (out of scope for sqlink):** the
`sys:compose@1.0.0` `PlanV1` schema needs a way to express
"explicit re-export of this instance from a named sub-component".
The wac surface area is

```wac
let lib = new sqlite:wasm-lib { ... };
export lib["sqlink:wasm/dispatch-bridge@0.1.0"];
```

— compose-core's `EmitHandler::compose_with_wrapper` would need to
build a top-level synthetic component (rather than directly using
the root component's bytes) that includes both the cli + lib as
instances and alias-exports the requested ones. That's a
non-trivial extension to wasm-compose or a switch to a different
composer backend (e.g. wac-graph as a library).

## Gap 2 — `composectl emit` doesn't unify versioned WASI imports across components

**Hits A1 (sqlink composed runtime).** **RESOLVED upstream**
alongside Gap 1 (`2e3ee85f`): the wac-graph library emit path
runs `TypeAggregator` before the outer world is encoded, which
merges semver-compatible imports to the highest common name.
Plans without `explicit_exports` still take the wasm-compose
wrapper fallback for now; that path retains the duplication and
is intentionally unused by sqlink.

sqlite-lib is compiled against WASI 0.2.4. sqlite-cli is compiled
against WASI 0.2.6. They're hosted by the same embedder; the
embedder satisfies both at the cli's version (the lib's WASI
calls work because the WASI surface is backwards-compatible across
0.2.x).

wac compose collapses these to ONLY 0.2.6 imports in the composed
output's outer world (correctly: the embedder only needs to
provide one set of WASI bindings).

composectl emit / wasm-compose KEEPS BOTH versions in the
composed output:

```
import wasi:cli/environment@0.2.4;
import wasi:cli/environment@0.2.6;
import wasi:cli/exit@0.2.4;
import wasi:cli/exit@0.2.6;
... (and so on for every WASI interface, ~22 duplicates total)
```

The artifact loads only against a host that provides BOTH 0.2.4
AND 0.2.6 wired through. sqlink-host today only wires through one
version per family.

This is the same upstream behavior as wac plug (which is why we
went to wac compose with the explicit recipe). The fix is the
same as gap 1: composectl needs the equivalent of wac compose's
explicit-instance-wiring + version-unification logic. wasm-compose
alone can't do it.

## Gap 3 — 100MB blob-store ceiling blocks postgis composition

**Hits A2 (postgis + mobilitydb shim composition).** **RESOLVED
upstream** in `58ce66f0`
(`feat(composectl): add --max-blob-size flag +
COMPOSECTL_MAX_BLOB_SIZE env`). Default raised to 1 GiB for the
`composectl` build-tool; DOS hedge remains on the
`compose-orchestrator-wasm` server tier. Verified locally:
`composectl blob put ~/git/postgis-wasm/postgis-composed.wasm`
(116 MiB) succeeds cleanly.

`composectl blob put postgis-composed.wasm`:

```
Error: BlobIoError: blob size 112512099 exceeds maximum 104857600
```

`postgis-composed.wasm` is the precomposed Geos + PROJ + PostGIS
runtime built in `~/git/postgis-wasm`. It's 112 MiB. The blob
store has a hardcoded 100 MiB limit in `SystemLimits::default()` /
`HostConfig { max_blob_size: 100 * 1024 * 1024 }` and `composectl`
exposes no CLI flag to override.

The cap is a sensible DOS hedge for `compose:store` API consumers
but the wrong default for build-time tooling that operates on
trusted local files.

**Upstream fix shape:**

- Add a `--max-blob-size` flag to `composectl` (or honour an env-var
  override such as `COMPOSECTL_MAX_BLOB_SIZE`).
- Raise the default for `composectl` specifically to (say) 1 GiB —
  the build tool is not a multi-tenant service.
- Or expose `HostConfig::max_blob_size` to programmatic consumers
  (sqlink could then drive emit via the lib API once we move to
  the Cargo-path-dep variant of the dep model).

mobilitydb is the same shape: it needs both
`postgis-composed.wasm` (112 MiB) AND `mdb-temporal-wasm.wasm`
(4.8 MiB), so the 100 MiB ceiling blocks both shim compositions.

## Test artifacts

The following commands reproduce the gap-1 and gap-2 evidence on a
machine with current sqlink + composectl builds:

```sh
TEST_DIR=$(mktemp -d)
cd "$TEST_DIR"

SQLITE_CLI=~/git/sqlink/target/wasm32-wasip2/release/sqlite_cli.component.wasm
SQLITE_LIB=~/git/sqlink/sqlite-wasm/target/wasm32-wasip2/release/sqlite_lib.component.wasm
COMPOSECTL=~/git/webassembly-component-orchestration/target/release/composectl

# Reference (wac compose)
wac compose ~/git/sqlink/composition-cli-sqlite-lib.wac \
    -d "sqlite:wasm-lib=$SQLITE_LIB" \
    -d "sqlite:cli=$SQLITE_CLI" \
    -o wac-runtime.wasm

# composectl emit
"$COMPOSECTL" blob put "$SQLITE_CLI"  # records digest
"$COMPOSECTL" blob put "$SQLITE_LIB"  # records digest

cat > plan.json <<EOF
{
  "version": "1",
  "root": "sqlite-cli",
  "components": [
    {"id": "sqlite-cli", "digest": [...sha256 of cli...]},
    {"id": "sqlite-lib", "digest": [...sha256 of lib...]}
  ],
  "bindings": [{
    "consumer_id": "sqlite-cli",
    "import_name": "sqlite:extension/spi@0.1.0",
    "provider_id": "sqlite-lib",
    "export_name": "sqlite:extension/spi@0.1.0"
  }],
  "secrets": [],
  "policy": {"determinism": "relaxed", "capabilities": [], "limits": {}}
}
EOF

"$COMPOSECTL" emit build plan.json --output composectl-runtime.wasm
diff <(wasm-tools component wit composectl-runtime.wasm | grep -E "^  (import|export) " | sort -u) \
     <(wasm-tools component wit wac-runtime.wasm        | grep -E "^  (import|export) " | sort -u)
```

The diff reproduces gap 1 (missing re-exports) and gap 2
(duplicate 0.2.4 + 0.2.6 imports) in a few lines.

## Resolution — done

All three gaps closed upstream (see per-gap references above).
On the sqlink side:

1. `composition-plans/sqlink-runtime.plan.json` now carries
   `explicit_exports` for `sqlite:extension/types@1.0.0` +
   `sqlink:wasm/dispatch-bridge@0.1.0` from the sqlite-lib
   sub-component.
2. `scripts/build-composed-runtime.sh` and
   `scripts/build-composed-runtime-single-memory.sh` gained a
   `SQLINK_COMPOSE_TOOL=composectl|both|wac` switch. In
   `both` mode the two artifacts are produced and their WIT
   surfaces diffed. In `composectl` mode the composectl artifact
   is promoted to the canonical output path.
3. The legacy `ORCHESTRATION_CROSS_CHECK=1` env-var is retained
   as an alias for `SQLINK_COMPOSE_TOOL=both`.

Next steps (per PLAN-orchestration-integration.md):

- One release of `SQLINK_COMPOSE_TOOL=both` in CI; then drop
  wac.
- Migrate the dep model from sibling-checkout to the
  full-Cargo-path-dep or vendored-submodule variant once CI
  hermeticity demands it.
