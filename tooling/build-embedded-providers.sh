#!/usr/bin/env bash
# Build the dot-command extension providers the cli bakes in via include_bytes!
# (#220: the cli must embed the PLUGGED <name>-provider.wasm — with the
# compose:dynlink/endpoint export — not the raw component, or auto-load fails
# with "not a compose:dynlink provider"). Places each at
# extensions/<name>/<name>-provider.wasm (gitignored *.wasm), which cli/src/lib.rs
# include_bytes!'s. Run BEFORE building the cli component.
set -uo pipefail
R="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="$R/target/wasm32-wasip2/release"

# The dot-command extensions auto-loaded by embed_core_dotcmd() in cli/src/lib.rs.
EXTS=(
  core-dotcmd sqlink-meta-cli sha3sum-cli serialize-cli archive-cli session-cli
  sqlite-utils-schema sqlite-utils-data sqlite-utils-fts sqlite-utils-maint
  bundle-cli prefix-cli
)

fail=0
for name in "${EXTS[@]}"; do
  u="${name//-/_}"
  if bash "$R/tooling/smoke-build-provider.sh" "$name" >/dev/null 2>&1; then
    cp "$TARGET_DIR/${u}_provider.wasm" "$R/extensions/$name/$name-provider.wasm" \
      && echo "  OK  $name-provider.wasm" \
      || { echo "  COPY-FAIL $name" >&2; fail=1; }
  else
    echo "  BUILD-FAIL $name (run: tooling/smoke-build-provider.sh $name)" >&2
    fail=1
  fi
done
[ "$fail" = 0 ] && echo "all embedded providers built" || echo "some providers failed" >&2
exit "$fail"
