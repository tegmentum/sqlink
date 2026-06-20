#!/usr/bin/env bash
# Upload built .component.wasm artifacts to Cloudflare R2.
#
# Usage:
#   R2_BUCKET=sqlite-wasm-extensions \
#   R2_ACCOUNT_ID=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
#   R2_ACCESS_KEY_ID=xxxxx \
#   R2_SECRET_ACCESS_KEY=yyyyyyy \
#   extensions-site/sync-r2.sh
#
# Optional env:
#   R2_PUBLIC_BASE  the public URL that maps to this bucket.
#                   Used to print the artifact URL of each upload.
#                   E.g. https://r2.sqlite-wasm.dev
#   DRY_RUN=1       print what would happen; don't upload
#
# Tooling:
#   Uses the AWS CLI v2 with R2's S3-compatible endpoint. R2 supports
#   the full AWS Signature V4 protocol. Get an API token from
#   Cloudflare dashboard  R2  Manage R2 API Tokens.
#
# Object layout (matches artifact_url emitted by build_registry.py):
#   extensions/<name>/<name>-<version>.component.wasm
#
# Idempotent: --metadata sha256:<HEX> means the sync compares object
# metadata server-side before re-uploading. Re-running with no
# changes uploads zero bytes.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

: "${R2_BUCKET:?R2_BUCKET env var is required}"
: "${R2_ACCOUNT_ID:?R2_ACCOUNT_ID env var is required}"
: "${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID env var is required}"
: "${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY env var is required}"

R2_ENDPOINT="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"
DRY="${DRY_RUN:-0}"
PUBLIC_BASE="${R2_PUBLIC_BASE:-}"

# Configure aws-cli for this invocation. R2 wants region us-east-1
# (the literal string  it's S3-compat, not actually us-east-1).
export AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID"
export AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY"
export AWS_DEFAULT_REGION=us-east-1
export AWS_EC2_METADATA_DISABLED=true   # no IMDS lookups

count=0
uploaded=0
skipped=0
total_bytes=0

# Walk every extension that has a built component artifact. Match
# the same path the build_registry.py + scan.py pair finds.
while IFS= read -r -d '' wasm; do
    rel="${wasm#$REPO_ROOT/}"
    # Extract the extension name from extensions/<name>/target/...
    name="$(echo "$rel" | awk -F/ '{print $2}')"

    # Read version from Cargo.toml. Tolerant of [package] subsection.
    cargo="$REPO_ROOT/extensions/$name/Cargo.toml"
    if [ ! -f "$cargo" ]; then
        echo "skip $name: no Cargo.toml" >&2
        continue
    fi
    version=$(awk -F\" '/^version *=/{print $2; exit}' "$cargo")
    if [ -z "$version" ]; then
        echo "skip $name: no version in Cargo.toml" >&2
        continue
    fi

    key="extensions/$name/$name-$version.component.wasm"
    size=$(stat -f%z "$wasm" 2>/dev/null || stat -c%s "$wasm")
    sha=$(shasum -a 256 "$wasm" | awk '{print $1}')

    count=$((count + 1))
    total_bytes=$((total_bytes + size))

    if [ "$DRY" = "1" ]; then
        echo "DRY:  s3://${R2_BUCKET}/${key}  (${size} bytes, sha256:${sha:0:12}...)"
        continue
    fi

    # Check if the object already exists with the same sha256.
    existing_sha=$(aws s3api head-object \
        --endpoint-url "$R2_ENDPOINT" \
        --bucket "$R2_BUCKET" \
        --key "$key" \
        --query 'Metadata.sha256' \
        --output text 2>/dev/null || echo "")

    if [ "$existing_sha" = "$sha" ]; then
        skipped=$((skipped + 1))
        echo "SKIP  $key  (sha256 unchanged)"
        continue
    fi

    aws s3 cp "$wasm" "s3://${R2_BUCKET}/${key}" \
        --endpoint-url "$R2_ENDPOINT" \
        --content-type "application/wasm" \
        --cache-control "public, max-age=31536000, immutable" \
        --metadata "sha256=${sha},size=${size}" \
        --no-progress

    uploaded=$((uploaded + 1))
    if [ -n "$PUBLIC_BASE" ]; then
        echo "  -> ${PUBLIC_BASE}/${key}"
    fi
done < <(find extensions -name '*_extension.component.wasm' -path '*/release/*' -print0 | sort -z)

echo
echo "Summary:"
echo "  total artifacts: $count"
echo "  uploaded:        $uploaded"
echo "  skipped (clean): $skipped"
echo "  total size:      $(numfmt --to=iec --suffix=B "$total_bytes" 2>/dev/null || echo "${total_bytes}B")"
if [ "$DRY" = "1" ]; then
    echo "  (DRY_RUN  no actual uploads)"
fi
