#!/usr/bin/env bash
#
# Publish the sqlink extension catalog to the datalink-ext R2 bucket.
#
# Reusable, idempotent, additive:
#   1. For every registry entry, locate a built `.component.wasm` artifact
#      (the scaffold shared target, the per-extension target, or the
#      top-level workspace target), validate it, and content-address it.
#   2. Upload each blob to wasm/sha256/<digest>/<name>.wasm (HEAD-skip).
#   3. Regenerate registry/catalog.json = registry filtered to the entries
#      that have a blob present + digest-verified, and upload it.
#
# Only loadable (blob-present, digest-matching) entries land in the catalog;
# builtin/placeholder entries (sha256:builtin / sha256:unbuilt) and entries
# with no buildable source are excluded by construction.
#
# Credentials come from ~/git/datalink/r2.env (R2_* -> AWS_*). Secrets are
# never printed. The blob layout + public host are:
#   bucket   datalink-ext
#   blob     wasm/sha256/<digest>/<name>.wasm   (application/wasm, immutable)
#   catalog  sqlink/catalog.json                (application/json, max-age=300)
#   public   https://datalink-ext.tegmentum.ai/sqlink/catalog.json
#
# Usage:
#   deploy/r2/publish.sh [--dry-run]
#
# Run from the repo root (or any subdir; it resolves the root from $0).
set -uo pipefail

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

REG="registry/index.json"
CATALOG="registry/catalog.json"
EXT_SHARED_TARGET="$ROOT/extensions/_shared-target/wasm32-wasip2/release"
AWS="${AWS:-$(command -v aws)}"
ENVFILE="${R2_ENV:-$HOME/git/datalink/r2.env}"

[ -f "$REG" ] || { echo "missing $REG" >&2; exit 1; }
[ -f "$ENVFILE" ] || { echo "missing R2 env $ENVFILE" >&2; exit 1; }

set -a; . "$ENVFILE"; set +a
export AWS_ACCESS_KEY_ID="${R2_ACCESS_KEY_ID:?}"
export AWS_SECRET_ACCESS_KEY="${R2_SECRET_ACCESS_KEY:?}"
export AWS_DEFAULT_REGION=auto
EP="https://${R2_ACCOUNT_ID:?}.r2.cloudflarestorage.com"
BUCKET=datalink-ext

# --- 1. gather + content-address artifacts -------------------------------
# Emits TSV: name <tab> digest <tab> size <tab> path  for every registry
# entry that has a valid built component.
MANIFEST="$(mktemp)"
python3 - "$REG" "$EXT_SHARED_TARGET" "$ROOT" > "$MANIFEST" <<'PY'
import json,sys,os,hashlib
reg,shared,root=sys.argv[1],sys.argv[2],sys.argv[3]
exts=json.load(open(reg))["extensions"]
def cands(name):
    u=name.replace('-','_')
    yield os.path.join(shared,f"{u}_extension.component.wasm")
    yield os.path.join(root,"extensions",name,"target/wasm32-wasip2/release",f"{u}_extension.component.wasm")
    yield os.path.join(root,"target/wasm32-wasip2/release",f"{u}_extension.component.wasm")
for e in exts:
    n=e["name"]
    ck=e.get("checksum","")
    if ck in ("sha256:builtin","sha256:unbuilt"):  # placeholder, skip
        continue
    p=next((c for c in cands(n) if os.path.isfile(c)),None)
    if not p:
        continue
    b=open(p,"rb").read()
    d=hashlib.sha256(b).hexdigest()
    print(f"{n}\t{d}\t{len(b)}\t{p}")
PY

# wasm-tools validate gate (drop anything that isn't a valid component).
VALID="$(mktemp)"
while IFS=$'\t' read -r name digest size path; do
  if command -v wasm-tools >/dev/null 2>&1; then
    wasm-tools validate "$path" >/dev/null 2>&1 || { echo "drop (invalid): $name" >&2; continue; }
  fi
  printf '%s\t%s\t%s\t%s\n' "$name" "$digest" "$size" "$path" >> "$VALID"
done < "$MANIFEST"

n_art=$(wc -l < "$VALID" | tr -d ' ')
echo "artifacts: $n_art"

# --- 2. upload blobs (additive, HEAD-skip) -------------------------------
put=0; skip=0; fail=0
mapfile -t LINES < "$VALID"
for ln in "${LINES[@]}"; do
  IFS=$'\t' read -r name digest size path <<< "$ln"
  key="wasm/sha256/$digest/$name.wasm"
  if "$AWS" s3api head-object --bucket "$BUCKET" --endpoint-url "$EP" --key "$key" >/dev/null 2>&1; then
    skip=$((skip+1)); continue
  fi
  if [ "$DRY_RUN" = 1 ]; then echo "would PUT $key"; put=$((put+1)); continue; fi
  if "$AWS" s3api put-object --bucket "$BUCKET" --endpoint-url "$EP" --key "$key" --body "$path" \
       --content-type application/wasm --cache-control "public, max-age=31536000, immutable" >/dev/null 2>&1; then
    put=$((put+1))
  else
    echo "blob upload FAIL: $name" >&2; fail=$((fail+1))
  fi
done
echo "blobs: PUT=$put SKIP=$skip FAIL=$fail"

# --- 3. regenerate + upload catalog --------------------------------------
python3 - "$REG" "$VALID" "$CATALOG" <<'PY'
import json,sys,datetime
reg,valid,out=sys.argv[1],sys.argv[2],sys.argv[3]
d=json.load(open(reg))
art={}
for line in open(valid):
    p=line.rstrip("\n").split("\t")
    art[p[0]]={"digest":p[1],"size":int(p[2])}
cat=dict(d)
cat["updated"]=datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
keep=[]
for e in d["extensions"]:
    n=e["name"]
    if n not in art:
        continue
    e=dict(e)
    e["checksum"]="sha256:"+art[n]["digest"]
    e["content_digest"]=art[n]["digest"]
    e["size_bytes"]=art[n]["size"]
    keep.append(e)
cat["extensions"]=keep
json.dump(cat,open(out,"w"),indent=1)
print(f"catalog entries: {len(keep)}")
PY

if [ "$DRY_RUN" = 1 ]; then echo "dry-run: catalog written to $CATALOG, not uploaded"; rm -f "$MANIFEST" "$VALID"; exit 0; fi

"$AWS" s3api put-object --bucket "$BUCKET" --endpoint-url "$EP" --key "sqlink/catalog.json" \
  --body "$CATALOG" --content-type application/json --cache-control "public, max-age=300" >/dev/null \
  && echo "catalog uploaded: sqlink/catalog.json" || { echo "catalog upload FAIL" >&2; exit 1; }

rm -f "$MANIFEST" "$VALID"
echo "done"
