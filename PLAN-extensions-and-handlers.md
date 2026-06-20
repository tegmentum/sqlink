# Plan: extensions + handlers expansion

> **Status: drafted 2026-06-20, ready to execute.**
> Eight new SQLite extensions and three new sqlite-wasm-httpd
> handlers. Each item sized S (<3h) / M (half-day) / L (~1d).
> JWT is the only hard dependency  the auth handler builds on
> it, so order matters there; everything else can run in any
> order or in parallel.

## Cross-cut: the scaffold every item shares

The catalog has 70+ extensions already; every one of them lands
through the same pattern. None of the items below need new
infrastructure  the work is in the per-extension body.

For SQLite extensions:
- New crate `extensions/NAME/` with `Cargo.toml` (`embed` feature,
  cdylib + rlib crate-type, deps on `wit-bindgen` + `wit-bindgen-rt`,
  optionally `sqlite-embed`)
- `src/lib.rs` with `wit_bindgen::generate!` against the right
  world (`tabular` for scalars/aggregates/vtabs, `stateful` for
  aggregates only, etc) + Guest impl
- `src/embed.rs` exposing `register_into(db: *mut sqlite3)` so the
  cli's embed path can call in natively (faster than the WIT path
  for hot paths)
- `smoke.sql` + `smoke.expected` so `make ext-smoke-all` catches
  regressions
- If it ships in the cli embed bundle: line in
  `sqlite-cli-embedded/Cargo.toml` + the embed registry

For wasm handlers:
- New crate `sqlite-wasm-httpd/handlers/NAME/` (standalone
  workspace) with `Cargo.toml`, `.cargo/config.toml` (target
  wasm32-wasip2), `build.sh` (cargo build + wasm-tools component
  new + wasi-p1 reactor adapter)
- `src/lib.rs` implementing `runtime.execute(source_name, source)`
  against `language-runtime` world
- README snippet showing how to load + a curl example

Acceptance gate for every item: build clean, smoke green, README
section if user-facing.

---

## SQLite extensions

### 1  `jwt`  M

**Goal.** Sign and verify JSON Web Tokens from SQL. Pairs with
the new httpd routing story: gate routes behind a token without
spawning a process.

**Functions.**
```
jwt_encode(header_json, payload_json, key, alg) -> text
jwt_decode(token)                                -> json {header,payload}
jwt_verify(token, key, alg)                      -> integer (0 or 1)
jwt_payload(token)                               -> json    (no verify)
jwt_header(token)                                -> json    (no verify)
```

**Algorithms (v1).** HS256, HS384, HS512 (via `hmac` +
`sha2`); Ed25519 (via `ed25519-dalek`). RS256/384/512 deferred
to v2 unless a consumer asks  pulls in `rsa` crate which has
real binary-size cost and tightening considerations.

**Dependencies.** `hmac` 0.12, `sha2` 0.10, `ed25519-dalek` 2,
`base64` 0.22, `serde_json` 1 (or hand-rolled JSON if size
matters; the cli already pays for serde_json elsewhere).

**Scope.** ~1 day for HS + Ed25519 + embed path + smokes.
RS256 +0.5 day.

**Acceptance.**
- HS256 round-trip: encode  decode  payload matches
- Bad signature: `jwt_verify()` returns 0
- Wrong algorithm in header: decode succeeds, verify fails
- Tampered payload: verify fails
- Ed25519 round-trip with a known test vector from RFC 8037

---

### 2  `hashes-fast`  S  (xxhash + murmur3)

**Goal.** Fast non-crypto hashes. sha3 is great for integrity;
this is for bloom filters, sharding, consistent hashing  the
sub-nanosecond regime where SHA3 is 100x too slow.

**Bundling rationale.** xxhash + murmur3 are the same shape and
the same user. Keep them in one extension so users don't have to
load two for "I just want a fast hash."

**Functions.**
```
xxh3(value)                                      -> integer (64-bit)
xxh3_128(value)                                  -> blob    (16 bytes)
xxh64(value, [seed])                             -> integer
xxh32(value, [seed])                             -> integer (32-bit)
murmur3_32(value, [seed])                        -> integer
murmur3_128(value, [seed])                       -> blob    (16 bytes)
```

**Dependencies.** `xxhash-rust` 0.8 (pure rust, fast, no_std),
`murmur3` 0.5 or `fasthash` for the murmur3 impl.

**Scope.** ~3 hours.

**Acceptance.**
- Known test vectors match published xxh3 / xxh64 / murmur3
  reference values
- Deterministic across runs (same input  same output)
- Smoke covers integer + text + blob inputs

---

### 3  `ulid`  S

**Goal.** Sortable 128-bit IDs. Where uuid v4 is random and
unsortable, ULID has a 48-bit timestamp prefix  ideal for
primary keys you also want to range-scan by creation time.

**Functions.**
```
ulid()                  -> text   (Crockford base32, 26 chars)
ulid_blob()             -> blob   (16 bytes, big-endian)
ulid_from(epoch_ms)     -> text   (encode a specific timestamp)
ulid_timestamp(ulid)    -> integer (extract ms epoch)
ulid_random_part(ulid)  -> blob   (10 bytes of randomness)
```

**Dependencies.** `ulid` crate (pure rust, ~50 LoC equivalent
work).

**Scope.** ~3 hours.

**Acceptance.**
- Generated ULID parses back via `ulid_timestamp`
- Two ULIDs generated 1ms apart sort in time order
- ULID format conforms to Crockford base32 spec

---

### 4  `nanoid`  S

**Goal.** URL-safe short IDs (default 21 chars). Lighter than
uuid for ID columns that just need uniqueness, not 128-bit
collision resistance.

**Functions.**
```
nanoid()                          -> text  (21 chars, URL-safe)
nanoid_n(len)                     -> text  (custom length)
nanoid_alpha(len, alphabet)       -> text  (custom alphabet)
```

**Dependencies.** `nanoid` crate (small, pure rust).

**Scope.** ~2 hours.

**Acceptance.**
- Default `nanoid()` is 21 chars, all in URL-safe alphabet
- `nanoid_alpha(8, 'abc')` only emits chars from 'abc'
- 10,000 nanoids in a smoke: no collisions

---

### 5  `uuid-v7`  S  (extend existing `uuid` extension)

**Goal.** Add the sortable UUID variant to the existing extension.
v7 is the modern alternative to v4 for primary keys.

**Functions (added to existing extension).**
```
uuid_v7()                  -> text   (RFC 9562 v7)
uuid_v7_blob()             -> blob   (16 bytes)
uuid_v7_timestamp(uuid)    -> integer (extract ms epoch)
```

**Dependencies.** `uuid` crate with `v7` feature  presumably
already in `extensions/uuid/Cargo.toml` with `v4` only. Bump.

**Scope.** ~2 hours.

**Acceptance.**
- v7 UUIDs are sortable by generation order
- `uuid_v7_timestamp()` returns the embedded ms epoch
- Existing v4 / v5 / nil functions unchanged

---

### 6  `lz4`  S

**Goal.** LZ4 compression  fast (1+ GB/s) at moderate ratio.
Different operating point from miniz/deflate (slower, better
ratio).

**Functions.**
```
lz4_compress(data)                -> blob   (LZ4 frame format)
lz4_decompress(blob)              -> blob
lz4_compress_raw(data)            -> blob   (raw block format)
lz4_decompress_raw(blob, max_out) -> blob   (raw block; needs size hint)
```

**Dependencies.** `lz4_flex` (pure rust, no_std-friendly,
~~100 KB of code).

**Scope.** ~3 hours.

**Acceptance.**
- Round-trip: `lz4_decompress(lz4_compress(x)) == x` for random
  blobs from 1 B to 1 MB
- Frame-format output is recognized by the `lz4` CLI on disk
- NULL input returns NULL

---

### 7  `zstd`  M  (pending toolchain probe)

**Goal.** Modern compression with the best ratio/speed tradeoff
in the workspace. Compress at level 3 (default) for ~3x deflate's
ratio at deflate's speed; level 19 for ~5x at slower speeds.

**Functions.**
```
zstd_compress(data, [level])  -> blob
zstd_decompress(blob)         -> blob
zstd_compress_dict(data, dict_blob, [level]) -> blob
zstd_decompress_dict(blob, dict_blob)        -> blob
```

**Toolchain risk.** `zstd` rust crate wraps `libzstd-sys` (C).
We need to verify it cross-compiles to wasm32-wasip2 against the
wasi-sdk clang. If it doesn't, fall back to `ruzstd` (pure-rust
decoder only) and either:
  (a) ship decompress-only,
  (b) port the zstd reference encoder (multi-week, out of scope), or
  (c) defer zstd entirely

**Toolchain probe step.** Before committing to the full
implementation, run:
```
cd /tmp && cargo new --lib zstd-probe && cd zstd-probe
echo 'zstd = "0.13"' >> Cargo.toml
RUSTFLAGS='-C link-arg=--no-entry' \
CC_wasm32_wasip2=$HOME/wasi-sdk/bin/clang \
AR_wasm32_wasip2=$HOME/wasi-sdk/bin/ar \
cargo build --target wasm32-wasip2 --release 2>&1 | tail -20
```
If it builds: proceed with full impl. If not: ship decompress-only
behind `ruzstd` with a documented "encode TBD" gap.

**Dependencies.** `zstd` 0.13 + `libzstd-sys` (if cross-compiles)
OR `ruzstd` (if not).

**Scope.** ~half day if zstd works; ~3 hours for decode-only via
ruzstd; ~1 day if we need to investigate alternatives.

**Acceptance (full path).**
- Round-trip at levels 1, 3, 19
- Dict path: compress with dict, decompress with same dict, bytes
  match
- Compress level 0 (auto = 3) matches level 3 output bit-for-bit
- Cross-implementation: a blob compressed with the zstd CLI
  decompresses via `zstd_decompress`

---

### 8  `h3`  M

**Goal.** Uber's H3 hexagonal hierarchical geospatial index.
Discrete global grid system for spatial joins, k-ring neighbor
queries, multi-resolution aggregation.

**Functions.**
```
h3_latlng_to_cell(lat, lng, resolution)   -> integer (H3Index as i64)
h3_cell_to_latlng(cell)                   -> text "lat,lng"
h3_cell_to_boundary(cell)                 -> json (list of lat,lng)
h3_cell_resolution(cell)                  -> integer (0-15)
h3_cell_parent(cell, resolution)          -> integer
h3_cell_children(cell, resolution)        -> json (list of cells)
h3_neighbors(cell)                        -> json (list of 6 cells)
h3_k_ring(cell, k)                        -> json (list of cells)
h3_distance(cell_a, cell_b)               -> integer (or NULL if not comparable)
h3_is_valid(cell)                         -> integer (0 or 1)
```

**Dependencies.** `h3o` (pure rust port of Uber's H3, BSD-3).
No C dep  clean wasm32-wasip2 build.

**Scope.** ~half day. h3o's API maps almost 1:1 to the function
list; most of the work is JSON serialization of the cell-list
returns.

**Acceptance.**
- Resolution 9 cell for (37.7749, -122.4194) matches the known
  H3 reference (`8928308280fffff`)
- `h3_distance(cell, cell) == 0`
- `h3_cell_children(cell, r+1)` returns 7 cells (the children
  count at one finer resolution)
- Boundary has 6 vertices for non-pentagon cells

---

### 9  `s2`  M

**Goal.** Google's S2 geometry library. Different (and historically
older) discrete global grid than H3; preferred for region queries
and large-scale coverings.

**Functions.**
```
s2_latlng_to_cell(lat, lng, level)        -> integer (S2CellId as i64)
s2_cell_to_latlng(cell)                   -> text "lat,lng"
s2_cell_to_token(cell)                    -> text   (hex token)
s2_token_to_cell(token)                   -> integer
s2_cell_level(cell)                       -> integer (0-30)
s2_cell_parent(cell, level)               -> integer
s2_cell_children(cell)                    -> json (list of 4 cells)
s2_covering(json_polygon, max_cells)      -> json (list of cells)
```

**Dependencies.** `s2` crate (pure rust port). Check status 
the S2 ecosystem has multiple competing rust crates; pick the
most-maintained.

**Scope.** ~half day.

**Acceptance.**
- Level-12 cell for (37.7749, -122.4194) parses back to the same
  lat/lng within S2's stated precision
- `s2_cell_children()` returns 4 cells
- `s2_covering()` of a small box returns < `max_cells` cells

---

## Wasm handlers

### 10  `auth`  S  (depends on `jwt` extension)

**Goal.** Verifies a JWT (or HMAC signature) on the inbound
request, returns 401 if invalid, 200 + the decoded claims as JSON
if valid. The downstream SQL route consumes the claims via `:body`
to drive AuthZ decisions.

**Operational model.** The auth handler doesn't *route*  it
*validates*. Pattern: register two routes for the same path,
one with `kind='wasm', handler='auth'` and one with `kind='sql'`
that runs the actual business logic. Route priority puts auth
first; on 401, the chain stops. (Router needs a "fall through
on 2xx" semantic  see Open Question below.)

**Open question (router contract).** The current router fires
exactly one matched handler per request. To get auth-then-sql
working we need either:
  (a) Chained handlers (router extension: `priority DESC, then
      take the highest non-rejecting result`), or
  (b) Wasm route inlines the post-auth SQL (auth handler talks
      back into the parent's sqlite via an SPI), or
  (c) The auth handler returns a "passthrough" signal the
      router interprets as "look up the next-best route."

(a) is the cleanest for v1  one new router rule, no new contract.
Document as "wasm route returning `pass` body + 200 falls through
to the next-best match."

**Dependencies.** `jwt` extension (item 1) loaded inside the
component, or the JWT crate inlined into the handler.

**Scope.** ~3 hours for the handler + ~3 hours for the router
"falls-through-on-pass" extension. Total ~half day.

**Acceptance.**
- Request with valid HS256 JWT in `Authorization: Bearer`: 200
  with claims in body
- Request with no header: 401
- Request with tampered JWT: 401
- Request with expired JWT (`exp` claim past): 401
- End-to-end: GET /protected with auth route in front, SQL route
  behind, returns the SQL result when token valid

---

### 11  `markdown`  S

**Goal.** POST markdown, get HTML. Useful for rendering user-
authored content stored in a SQL column, for static-site
generators driven by the database, or for the SQL routing case
where the response shape is naturally HTML.

**Crate.** `pulldown-cmark` (pure rust, no_std-friendly, ships
in many other rust HTTP servers).

**Behaviour.** Default to GFM (GitHub-Flavored Markdown) flavor;
no HTML pass-through by default (XSS safety). A `?safe=false`
query param enables raw HTML pass-through for trusted callers.

**Scope.** ~1 hour.

**Acceptance.**
- `POST /md` with `# hello` body returns `<h1>hello</h1>`
- Response ctype is `text/html; charset=utf-8`
- Raw `<script>` tag in input is escaped by default

---

### 12  `yaml-to-json`  S

**Goal.** Format conversion. Useful for config-as-data flows
where the user wants to author in YAML but the SQL handler
expects JSON.

**Functions surface.**
- POST /yaml2json: body is YAML, response is JSON
- POST /json2yaml (same crate, reverse direction): body is JSON,
  response is YAML

**Crate.** `serde_yaml` 0.9 + `serde_json` 1.

**Scope.** ~1 hour for the pair.

**Acceptance.**
- YAML `{a: 1, b: [2, 3]}` round-trips through JSON and back
- Invalid YAML returns 400 with the parse error in the body
- Non-UTF8 input returns 400

---

## Risks + open questions

| # | Risk | Mitigation |
|---|---|---|
| 7 | `zstd` C crate may not cross-compile to wasm32-wasip2 | Probe step before commit; fall back to `ruzstd` (decode-only) if it fails |
| 8/9 | `h3o` / `s2` add ~500 KB-1 MB of code to the component | Acceptable for opt-in `.load`; document size in the README |
| 10 | Router has no chain-on-success contract today | Item 10 includes the router change; documented as a new contract |
| 1 | Ed25519 key parsing has multiple wire formats (PKCS#8, PEM, raw 32-byte) | Document accepted formats; reject unknown ones with a clear error |

---

## Sequencing

The recommended order minimizes blocking and lets cheap wins
land early:

1. **Day 1 morning.** Items 2 + 3 + 4 + 5 (hashes-fast, ulid, nanoid,
   uuid-v7). All S; same scaffold; ~half day total. Ship as one
   batch commit "feat: ID/hash extensions".

2. **Day 1 afternoon.** Item 1 (jwt). M; unblocks item 10.

3. **Day 2 morning.** Items 6 + 11 + 12 (lz4, markdown,
   yaml-to-json). All S, no interdependencies; parallel-friendly.

4. **Day 2 afternoon.** Item 7 (zstd) probe; commit decode-only
   if encode toolchain fails, full path if it succeeds.

5. **Day 3 morning.** Item 10 (auth handler + router chain rule).

6. **Day 3 afternoon.** Items 8 + 9 (h3 + s2).

7. **Day 4.** Buffer for any item's smokes, cli embed bundle line,
   README refresh.

**Total budget:** ~4 dev days for the full set, executed
sequentially. Parallel agents could compress to ~2 days; the
unique-extension-pattern shape means an agent per item works
cleanly.

---

## What this plan does NOT include (deliberate)

- Argon2 / bcrypt / pbkdf2 password hashing  separate plan
  (deserves its own thought on side-channel resistance + the
  per-platform CPU intensity tuning)
- RS256 JWT  v2 once an Ed25519-equivalent shipping plan is in
  place
- A "chain of N wasm handlers" router primitive  item 10 needs
  only "fall through on 200 + body='pass'", not a full chain
- Per-route capability policy (which handler can do what)  the
  current `Policy::deny_all` is fine for v1; surface comes later

---

## Acceptance for the plan itself

This plan is done when:
- Every item has a tracked task (TaskCreate) before work starts
- Each item's commit references its plan number ("perf: ULID
  generation (PLAN-extensions-and-handlers #3)")
- README + `MEMORY.md` reflect the post-plan state of the
  catalog and the handlers/ directory
- `make ext-smoke-all` is green with the new smokes included
