# Orchestration substrate gaps (blocks Tier 1 cutover)

Tier 1 of [`PLAN-orchestration-integration.md`](../plans/PLAN-orchestration-integration.md)
calls for `composectl emit` to run alongside `wac compose` /
`wac plug` as a parallel cross-check, then retire wac after a
soak. This document records the concrete substrate gaps surfaced
when running that cross-check on real sqlink inputs at the
current upstream tip (`~/git/webassembly-component-orchestration`,
non-tegmentum). The gaps land upstream, not in sqlink, but until
they're closed sqlink can't flip from `wac compose` to
`composectl emit` without regressing functionality.

The plan files in `composition-plans/` are written against the
declarative spec the substrate would need to express — they ride
ahead of the upstream work so we can swap in `composectl emit`
the moment the gaps close.

## Gap 1 — `composectl emit` cannot re-export non-root component instances

**Hits A1 (sqlink composed runtime).** Blocks cutover.

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

**Hits A1 (sqlink composed runtime).** Blocks cutover.

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

**Hits A2 (postgis + mobilitydb shim composition).** Blocks
cutover.

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

## Resolution path (out of sqlink's scope)

Per task #486 instructions ("if you hit a substrate blocker,
STOP and report"), we land:

- The plan-file declaratives that the substrate would need to
  emit from (so the work is ready the moment the gaps close).
- The build-script parallel cross-check, currently no-op'd with a
  comment pointing at this doc.
- This document, so a future cutover knows exactly what upstream
  capabilities to wait on.

Sqlink does NOT attempt to fix upstream (per the task standing
constraints) and does NOT carry a patched fork. When upstream
gains explicit alias-export support + version unification + a
configurable blob-size cap, we revisit:

1. Flip the build script to invoke `composectl emit` in parallel
   with `wac compose`.
2. Verify the two artifacts have identical world shapes via
   `wasm-tools component wit | diff`.
3. After one release of agreement, drop the wac path.

The plan files in `composition-plans/` are the load-bearing
deliverable that survives the wait.
