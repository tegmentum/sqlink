#!/usr/bin/env bash
# Render a shim composition plan by substituting placeholder digests
# in `composition-plans/{shim}-shim.plan.json` with real SHA-256
# digests of the input .wasm components.
#
# Motivation: `composition-plans/{postgis,mobilitydb}-shim.plan.json`
# ship as templates with intentional placeholder digest bytes
# (`[171, 205, 239, 0, 1, 2, ...]`) — the digests are
# build-time-dependent and must be filled in each release. See
# `composition-plans/README.md#digest-discipline` and Tier 1.1.b of
# `docs/plans/PLAN-orchestration-integration.md`.
#
# Usage:
#   scripts/render-shim-plan.sh <shim>
#
# where <shim> is one of:
#   - postgis      renders postgis-shim.plan.json
#   - mobilitydb   renders mobilitydb-shim.plan.json
#
# Output:
#   composition-plans/build/{shim}-shim.rendered.plan.json
#
# Optional environment overrides for peer-repo layouts:
#   POSTGIS_WASM_ROOT       (default: ~/git/postgis-wasm)
#   POSTGIS_BRIDGE_ROOT     (default: ~/git/postgis-sqlink-bridge)
#   MOBILITYDB_WASM_ROOT    (default: ~/git/mobilitydb-wasm)
#   MOBILITYDB_BRIDGE_ROOT  (default: ~/git/mobilitydb-sqlink-bridge)
#
# If `SQLINK_COMPOSECTL_EMIT=1` (or `--emit` is passed after the
# shim name) the script additionally calls `composectl emit build`
# on the rendered plan and writes the composed .wasm alongside as
# `composition-plans/build/{shim}-shim-composed.wasm`.
#
# References #823 (Tier 1.1.b render pipeline enablement).

set -euo pipefail

usage() {
    cat >&2 <<EOF
Usage: $0 <shim> [--emit]

  shim     one of: postgis, mobilitydb

  --emit   after rendering, invoke composectl emit build to
           produce composition-plans/build/{shim}-shim-composed.wasm.
           Same as SQLINK_COMPOSECTL_EMIT=1.

Environment overrides:
  POSTGIS_WASM_ROOT       default: ~/git/postgis-wasm
  POSTGIS_BRIDGE_ROOT     default: ~/git/postgis-sqlink-bridge
  MOBILITYDB_WASM_ROOT    default: ~/git/mobilitydb-wasm
  MOBILITYDB_BRIDGE_ROOT  default: ~/git/mobilitydb-sqlink-bridge
  COMPOSECTL              default: composectl (from PATH)
EOF
    exit 2
}

if [[ $# -lt 1 ]]; then usage; fi

SHIM="$1"
shift || true

EMIT="${SQLINK_COMPOSECTL_EMIT:-0}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --emit) EMIT=1; shift ;;
        -h|--help) usage ;;
        *) echo "unknown arg: $1" >&2; usage ;;
    esac
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLANS_DIR="$REPO_ROOT/composition-plans"
BUILD_DIR="$PLANS_DIR/build"

POSTGIS_WASM_ROOT="${POSTGIS_WASM_ROOT:-$HOME/git/postgis-wasm}"
POSTGIS_BRIDGE_ROOT="${POSTGIS_BRIDGE_ROOT:-$HOME/git/postgis-sqlink-bridge}"
MOBILITYDB_WASM_ROOT="${MOBILITYDB_WASM_ROOT:-$HOME/git/mobilitydb-wasm}"
MOBILITYDB_BRIDGE_ROOT="${MOBILITYDB_BRIDGE_ROOT:-$HOME/git/mobilitydb-sqlink-bridge}"

case "$SHIM" in
    postgis)
        PLAN_TEMPLATE="$PLANS_DIR/postgis-shim.plan.json"
        # component id  ->  input .wasm path
        # Per Tier 1 A2, the postgis shim wraps the postgis-composed
        # monolith even though postgis-wasm ships 4 per-sub-ext
        # providers (Phase 3). The monolith is the shim's declared
        # input in postgis-shim.plan.json; sub-ext provider fan-out
        # happens in postgis-wasm's own plan chain, not here.
        declare -A DEP_PATHS=(
            ["postgis-composed"]="$POSTGIS_WASM_ROOT/postgis-composed.wasm"
            ["postgis-sqlink-bridge"]="$POSTGIS_BRIDGE_ROOT/target/wasm32-wasip2/release/postgis_sqlink_bridge.wasm"
        )
        ;;
    mobilitydb)
        PLAN_TEMPLATE="$PLANS_DIR/mobilitydb-shim.plan.json"
        # The `mdb-temporal-wasm` component id in the plan refers to
        # the mobilitydb-composed-provider artifact (peer prereq per
        # the mobilitydb monolith design lock); pre-Phase 3 the id
        # matched the raw mdb-temporal-wasm build, but Tier 1 wraps
        # the composed provider.
        declare -A DEP_PATHS=(
            ["mdb-temporal-wasm"]="$MOBILITYDB_WASM_ROOT/mobilitydb-composed-provider.wasm"
            ["mobilitydb-sqlink-bridge"]="$MOBILITYDB_BRIDGE_ROOT/target/wasm32-wasip2/release/mobilitydb_sqlink_bridge.wasm"
        )
        ;;
    *)
        echo "unknown shim: $SHIM (expected: postgis, mobilitydb)" >&2
        usage
        ;;
esac

if [[ ! -f "$PLAN_TEMPLATE" ]]; then
    echo "plan template missing: $PLAN_TEMPLATE" >&2
    exit 1
fi

mkdir -p "$BUILD_DIR"

RENDERED_PLAN="$BUILD_DIR/${SHIM}-shim.rendered.plan.json"

# Serialize DEP_PATHS -> tmp JSON for python3 consumption.
DEPS_JSON="$(mktemp -t "sqlink-shim-deps-${SHIM}.XXXXXX.json")"
trap 'rm -f "$DEPS_JSON"' EXIT
{
    for id in "${!DEP_PATHS[@]}"; do
        printf '%s\t%s\n' "$id" "${DEP_PATHS[$id]}"
    done
} | python3 -c '
import json, sys
d = dict(line.rstrip("\n").split("\t", 1) for line in sys.stdin if line.strip())
json.dump(d, sys.stdout)
' > "$DEPS_JSON"

echo "==> Rendering $SHIM shim plan..."
echo "    template : $PLAN_TEMPLATE"
echo "    output   : $RENDERED_PLAN"

PLAN_TEMPLATE="$PLAN_TEMPLATE" \
DEPS_JSON="$DEPS_JSON" \
RENDERED_PLAN="$RENDERED_PLAN" \
python3 <<'PY'
import hashlib, json, os, sys

template_path = os.environ["PLAN_TEMPLATE"]
rendered_path = os.environ["RENDERED_PLAN"]
dep_map = json.load(open(os.environ["DEPS_JSON"]))

with open(template_path) as f:
    plan = json.load(f)

missing = []
declared = {c["id"] for c in plan["components"]}
unknown = sorted(declared - set(dep_map.keys()))
if unknown:
    sys.exit(
        f"plan declares component id(s) with no DEP_PATHS entry: {unknown}. "
        "Add a mapping in scripts/render-shim-plan.sh."
    )

for comp in plan["components"]:
    dep_path = dep_map[comp["id"]]
    if not os.path.exists(dep_path):
        missing.append(f"{comp['id']} @ {dep_path}")
        continue
    with open(dep_path, "rb") as f:
        digest = hashlib.sha256(f.read()).digest()
    comp["digest"] = list(digest)
    print(f"    {comp['id']:32s}  {digest.hex()}")

if missing:
    sys.exit(
        "missing input .wasm(s) — cannot render plan:\n  "
        + "\n  ".join(missing)
        + "\n(build the peer repos, or set the *_ROOT env vars to the correct location)"
    )

# Sort components by id for byte-stable rendered output. Keeps
# derived plan digest stable across runs when inputs are unchanged.
plan["components"].sort(key=lambda c: c["id"])

with open(rendered_path, "w") as f:
    json.dump(plan, f, indent=2)
    f.write("\n")
PY

echo "==> Wrote $RENDERED_PLAN"

if [[ "$EMIT" == "1" ]]; then
    COMPOSECTL="${COMPOSECTL:-composectl}"
    if ! command -v "$COMPOSECTL" &>/dev/null; then
        # Fall back to the sibling checkout's release build, matching
        # scripts/build-composed-runtime.sh's discovery pattern.
        ORCH_ROOT="${SQLINK_ORCH_ROOT:-$REPO_ROOT/../webassembly-component-orchestration}"
        if [[ -x "$ORCH_ROOT/target/release/composectl" ]]; then
            COMPOSECTL="$ORCH_ROOT/target/release/composectl"
        else
            echo "composectl not found on PATH; skipping --emit step." >&2
            echo "(build via: cd $ORCH_ROOT && cargo build --release -p composectl)" >&2
            exit 0
        fi
    fi

    echo "==> composectl plan validate $RENDERED_PLAN"
    "$COMPOSECTL" plan validate "$RENDERED_PLAN"

    # Stage the input blobs so emit can resolve them by content
    # address. `blob put` is idempotent; a re-run is a no-op.
    for id in "${!DEP_PATHS[@]}"; do
        path="${DEP_PATHS[$id]}"
        "$COMPOSECTL" blob put "$path" >/dev/null 2>&1 || true
    done

    OUT_WASM="$BUILD_DIR/${SHIM}-shim-composed.wasm"
    echo "==> composectl emit build -> $OUT_WASM"
    "$COMPOSECTL" emit build "$RENDERED_PLAN" -o "$OUT_WASM"
    echo "    wrote $(du -h "$OUT_WASM" | cut -f1) $OUT_WASM"
fi
