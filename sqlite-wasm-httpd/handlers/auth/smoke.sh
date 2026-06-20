#!/usr/bin/env bash
# End-to-end smoke for the auth wasm handler.
#
# Starts sqlite-wasm-httpd with this handler loaded, registers a
# /auth route, and curls through it with valid + invalid JWTs.
# Prints PASS / FAIL per case; exits nonzero on any failure.
#
# Assumes:
#   - ./build.sh has produced target/wasm32-wasip2/release/wasm_auth_handler.component.wasm
#   - sqlite-wasm-httpd built at ../../../target/debug/sqlite-wasm-httpd
#   - python3 available (test JWT generation)
set -uo pipefail
cd "$(dirname "$0")"

HTTPD="${HTTPD:-../../../target/debug/sqlite-wasm-httpd}"
COMP="target/wasm32-wasip2/release/wasm_auth_handler.component.wasm"
DB="/tmp/auth_smoke.db"
PORT="${PORT:-18099}"

if [ ! -x "$HTTPD" ]; then echo "no httpd binary: $HTTPD"; exit 1; fi
if [ ! -f "$COMP" ]; then echo "no component: $COMP  run ./build.sh"; exit 1; fi

TOKENS=$(python3 -c "
import hmac, hashlib, base64, json, time
def b64url(b): return base64.urlsafe_b64encode(b).rstrip(b'=').decode()
secret = b'secret'
header = {'alg':'HS256','typ':'JWT'}
payload = {'sub':'alice','role':'admin','exp':int(time.time())+3600}
h=b64url(json.dumps(header,separators=(',',':')).encode())
p=b64url(json.dumps(payload,separators=(',',':')).encode())
sig=hmac.new(secret,f'{h}.{p}'.encode(),hashlib.sha256).digest()
print('VALID='+f'{h}.{p}.{b64url(sig)}')
expired={'sub':'alice','exp':int(time.time())-60}
ep=b64url(json.dumps(expired,separators=(',',':')).encode())
esig=hmac.new(secret,f'{h}.{ep}'.encode(),hashlib.sha256).digest()
print('EXPIRED='+f'{h}.{ep}.{b64url(esig)}')
wsig=hmac.new(b'wrong',f'{h}.{p}'.encode(),hashlib.sha256).digest()
print('TAMPERED='+f'{h}.{p}.{b64url(wsig)}')
")
eval "$TOKENS"

rm -f "$DB"
"$HTTPD" --db "$DB" --init-routes --load "auth=$COMP" --port "$PORT" \
    > /tmp/httpd_auth_smoke.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null; rm -f "$DB"' EXIT
sleep 2

curl -s -X POST "http://localhost:$PORT/sql" -d \
    "INSERT INTO routes (method, pattern, handler, kind) VALUES \
     ('POST', '/auth', 'auth', 'wasm'), \
     ('GET', '/auth', 'auth', 'wasm')" > /dev/null

fail=0
check() {
    local name="$1" expected_status="$2" expected_substr="$3" actual_status="$4" actual_body="$5"
    if [ "$actual_status" = "$expected_status" ] && [[ "$actual_body" == *"$expected_substr"* ]]; then
        echo "PASS  $name  $actual_status $actual_body"
    else
        echo "FAIL  $name  got=$actual_status $actual_body  wanted=$expected_status containing $expected_substr"
        fail=1
    fi
}

run() {
    local method="$1" path="$2" body="$3"
    local resp
    resp=$(curl -sS -X "$method" "http://localhost:$PORT$path" -d "$body" -w '\n__STATUS__%{http_code}')
    echo "$resp"
}

# Test 1: no body  401 missing
out=$(run POST /auth ""); body="${out%$'\n__STATUS__'*}"; status="${out##*__STATUS__}"
check "no-token" "401" "missing token" "$status" "$body"

# Test 2: valid  200, claims with sub=alice
out=$(run POST /auth "$VALID"); body="${out%$'\n__STATUS__'*}"; status="${out##*__STATUS__}"
check "valid-raw" "200" '"sub":"alice"' "$status" "$body"

# Test 3: valid with Bearer prefix
out=$(run POST /auth "Bearer $VALID"); body="${out%$'\n__STATUS__'*}"; status="${out##*__STATUS__}"
check "valid-bearer" "200" '"role":"admin"' "$status" "$body"

# Test 4: query param token
resp=$(curl -sS "http://localhost:$PORT/auth?token=$VALID" -w '\n__STATUS__%{http_code}')
body="${resp%$'\n__STATUS__'*}"; status="${resp##*__STATUS__}"
check "valid-query" "200" '"sub":"alice"' "$status" "$body"

# Test 5: tampered signature
out=$(run POST /auth "$TAMPERED"); body="${out%$'\n__STATUS__'*}"; status="${out##*__STATUS__}"
check "tampered" "401" "bad signature" "$status" "$body"

# Test 6: expired
out=$(run POST /auth "$EXPIRED"); body="${out%$'\n__STATUS__'*}"; status="${out##*__STATUS__}"
check "expired" "401" "expired" "$status" "$body"

# Test 7: garbage
out=$(run POST /auth "not-a-jwt"); body="${out%$'\n__STATUS__'*}"; status="${out##*__STATUS__}"
check "garbage" "401" "missing token" "$status" "$body"

echo
if [ $fail -eq 0 ]; then
    echo "auth smoke: all pass"
    exit 0
else
    echo "auth smoke: FAILED"
    exit 1
fi
