# Plan: extension catalog follow-ups

> **Phase E1 status: shipped.** bloom + hyperloglog +
> count_min + closure (graph) + trie (prefix) all delivered.
> 11 native unit tests pass; end-to-end smoke verifies each
> extension through the cli. Pure-Rust deps only
> (twox-hash for hash kernels).
>
> **Phase E2 status: shipped.** codecs extension bundles all
> three  cbor + msgpack + yaml  into one .wasm (each codec
> is just a scalar pair, so a single load is the cleaner UX).
> Round-trip via serde_json::Value as the common interchange
> shape. 4 native unit tests pass; smoke shows JSON 31B 
> CBOR/MessagePack 16B on a 5-field test object.
>
> **Phase E3 status: shipped.** Two extensions  text-utils
> (sql_normalize scalar + prefixes eponymous TVF) and
> spellfix1 (Levenshtein edit-distance vtab over a TEXT
> column). Brute-force scan with early-exit banding; full
> recall, O(N) per query. 11 native unit tests pass; smoke
> verifies typo-tolerant lookup ("thier"  their/them/there/
> three/water).
>
> **Phase E4 status: shipped.** extensions/stats gains seven
> new aggregates: percentile_cont / percentile_disc, skewness
> / kurtosis (one-pass Pébay 2008 moments aggregator),
> regr_slope / regr_intercept / regr_r2 (closed-form least
> squares). 15 native unit tests pass (6 new); smoke verifies
> regression on y=2x+1 returns exact slope=2 intercept=1 r2=1.
>
> **Phase E5 status: shipped.** extensions/time ships eight
> scalars: date_trunc, iso_year / iso_week / iso_weekday,
> fiscal_year / fiscal_quarter (configurable start_month),
> business_days_between (Mon-Fri counting, signed by
> direction), weekday_name. chrono with `default-features =
> false` keeps the wasm size small. tz_convert and
> interval_add deferred  the former needs chrono-tz's ~1 MB
> tzdata; the latter is redundant with SQLite's datetime()
> modifier syntax. 7 native unit tests pass.
>
> **Phase E6 status: shipped.** extensions/crypto-auth bundles
> JWT (verify/decode_header/decode_payload), TOTP (RFC 6238
> with hand-rolled HMAC-SHA1 over base32-secret), Argon2
> (hash/verify), and bcrypt (hash/verify with cost arg).
> Components-wrap requires the wasi-preview1 adapter
> (argon2/bcrypt pull rand_core::OsRng  preview1
> random_get import). 6 native unit tests pass including
> the RFC 6238 reference vector at T=59  '94287082'.

## Goal

Ship the remaining well-known SQLite extension surfaces
identified in the catalog survey  ten focused phases, each
independently mergeable, ordered roughly by leverage-per-day.
Total estimated runway: ~30-40 days of focused work; most
phases are 1-4 days.

Pre-existing infrastructure these all ride on:
- Tabular world (vtab support) and stateful world (aggregate
  support)  see existing extensions/*/ examples.
- Host `dispatch_scalar` already routes through the
  appropriate cached Store based on the manifest's
  vtab/aggregate footprint.
- Capability gates (`declared_capabilities`) exist for
  scoping things like filesystem access  see how the bridge
  declares them; no new host machinery for E8 / E9 except
  the WASI HTTP wiring in E9.

---

## Phase E1  Probabilistic sketches & graph (~5-6 days)

Pure-Rust scalar/aggregate ports. No host changes; each
independently shippable.

### bloom (~1 day)
Scalar shape:
- `bloom_create(n_items, fp_rate)` → BLOB (sized filter)
- `bloom_add(filter, value)` → BLOB (new filter with value)
- `bloom_might_contain(filter, value)` → INTEGER (0/1)

Wire format: 16-byte header (m, k, n_added) + bit array. Two
hash functions via xxhash with different seeds (k = ceil(-ln(fp)/ln(2))).

Pairs naturally with vec0: pre-filter candidate rowids by a
bloom of "interesting" ids, then run the kNN.

### hyperloglog (~1 day)
Aggregate:
- `hll(value)` over a column → STATE (BLOB)
- `hll_cardinality(state)` → INTEGER

Standard implementation at p=14 (16 KB state, ~0.8% error).
Pure-Rust impl ~150 LOC; no crate dep needed (avoids
hyperloglog crate's deprecation drift).

### count_min (~1 day)
Aggregate + lookup:
- `count_min(value, k)` aggregate → STATE
- `count_min_estimate(state, value)` → INTEGER

Width = 2048 / depth = 4 default. Useful for top-k frequency
estimation on telemetry rollups.

### closure (~2 days)
Vtab port of `ext/misc/closure.c`. Graph closure across a
parent/child column pair:
```sql
CREATE VIRTUAL TABLE c USING closure(
  tablename='people', idcolumn='id', parentcolumn='parent'
);
SELECT * FROM c WHERE root=42 AND depth<5;
```
xBestIndex consumes `root`, `depth`, `tablename`, `idcolumn`,
`parentcolumn` as HIDDEN constraints (per upstream semantics).
xFilter does BFS via spi.execute. Existing vec0 wrapping
pattern is the template.

### trie (~1 day)
Vtab over a TEXT-keyed source:
```sql
CREATE VIRTUAL TABLE t USING trie(source=words, key_column=word);
SELECT word FROM t WHERE prefix='auto';
```
Build a radix-trie on first query, cache per-instance like
vec0. Optional `case_insensitive=true` arg. Pair with the
existing persistence shadow-table pattern.

**Open questions**: closure on cyclic graphs (cycle detection
via visited-set). trie cache invalidation on source-table
inserts (polling pattern from vec0 Phase 1).

---

## Phase E2  Data interchange codecs (~2 days)

Each ~half day. All scalar pairs:

### cbor
- `cbor_encode(json_text)` → BLOB
- `cbor_decode(blob)` → TEXT (canonical JSON)

`ciborium` crate (already in orchestrator side). Round-trip
through serde_json::Value.

### msgpack
- `msgpack_encode(json_text)` → BLOB
- `msgpack_decode(blob)` → TEXT

`rmp-serde` crate, same shape as cbor.

### yaml
- `yaml_to_json(yaml)` → TEXT
- `json_to_yaml(json)` → TEXT

`serde_yaml` crate.

### protobuf  **DEFERRED**
Needs a schema. Out of scope for v1; would be its own plan.

---

## Phase E3  Text utilities (~4 days)

### spellfix1 (~3 days)
Vtab port of SQLite's spellfix1 (mainline contrib). Levenshtein
edit-distance + phonetic key for typo-tolerant search:
```sql
CREATE VIRTUAL TABLE corpus USING spellfix1;
INSERT INTO corpus(word) SELECT word FROM dictionary;
SELECT word, distance FROM corpus
  WHERE word MATCH 'thier' AND distance<=2;
```
The persistence story is the same shadow-table pattern vec0
uses. Edit-distance kernel: pure-Rust Levenshtein with
Wagner-Fischer DP (cap at 2-3 max edits for early-exit speed).

### normalize (~half day)
Scalar:
- `sql_normalize(sql)` → TEXT (literals  `?`, names lowercased)

Used for query-plan cache keying. Tokenize-and-replace pass
over the SQL string; no parser needed for the simple form.

### prefixes (~half day)
Vtab:
```sql
SELECT prefix FROM prefixes('hello');
-- > 'h', 'he', 'hel', 'hell', 'hello'
```
Eponymous TVF; trivial generator pattern from
extensions/series.

---

## Phase E4  Statistics extensions (~2 days)

Extend the existing `extensions/stats/` with more aggregates;
no new crate scaffolding needed.

- **percentile_cont(p)** / **percentile_disc(p)**  continuous + discrete percentile aggregates. Sort the sample set in finalize; pick the bracketing pair for `_cont` (linear interpolation) or floor for `_disc`.
- **skewness / kurtosis**  third/fourth moments via Welford-equivalent online update.
- **linear_regression**  three companion aggregates: `regr_slope(y,x)`, `regr_intercept(y,x)`, `regr_r2(y,x)`. State: sum_x, sum_y, sum_xy, sum_x2, sum_y2, n.

All ~half day each except regression (1 day for the three
together).

---

## Phase E5  Time / date (~2 days)

Single new extension `extensions/time/` with scalars covering
gaps in SQLite builtins:

- `date_trunc(unit, ts)`  truncate to year/quarter/month/week/day/hour/minute
- `interval_add(ts, n, unit)`  add N units (sqlite has `datetime(ts, '+N days')` already; this is the typed form)
- `business_days_between(start, end, weekends, holidays)`  weekend mask + optional holiday list
- `iso_week(ts)` / `iso_year(ts)`  ISO 8601 week numbering
- `fiscal_year(ts, start_month)`  configurable fiscal year start
- `tz_convert(ts, from_tz, to_tz)`  IANA timezone conversion (chrono-tz)

`chrono` + `chrono-tz` crates. ~2 days total including
manifest + tests.

---

## Phase E6  Crypto / auth (~3 days)

Verify-only and stateless  no key storage in v1. All scalar.

### jwt (~1 day)
- `jwt_verify(token, jwk_json)` → INTEGER (0/1 + claims-not-expired check)
- `jwt_decode_header(token)` / `jwt_decode_payload(token)` → TEXT (JSON)

`jsonwebtoken` crate. Supports RS256/HS256/ES256 verification.

### totp (~half day)
- `totp(secret_base32, time)` → TEXT (6-digit code)
- `totp_verify(secret, code, time, window)` → INTEGER

HMAC-SHA1 over time/30. ~50 LOC.

### argon2 (~half day)
- `argon2_hash(password, salt)` → TEXT (encoded hash)
- `argon2_verify(hash, password)` → INTEGER

`argon2` crate. Default cost params suit web auth.

### bcrypt (~half day)
- `bcrypt_hash(password, cost)` → TEXT
- `bcrypt_verify(hash, password)` → INTEGER

`bcrypt` crate.

### shacrypt / scrypt  **DEFERRED**
Niche password-hash variants. Add on request.

---

## Phase E7  Web / parsing (~2 days)

### jsonpath (~1 day)
Scalars:
- `jsonpath(doc, '$.path.to[*]')` → TEXT (matched array JSON)
- `jsonpath_first(doc, expr)` → TEXT (single value)
- `jsonpath_exists(doc, expr)` → INTEGER

`jsonpath_lib` or `serde_json_path`. Fills the gap SQLite's
`json_extract` leaves (limited path syntax).

### html (~1 day)
Scalars:
- `html_extract(doc, css_selector)` → TEXT (matched text)
- `html_extract_all(doc, css_selector)` → TEXT (JSON array of matches)
- `html_attr(doc, selector, attr)` → TEXT

`scraper` crate (built on `html5ever`). Caveat: `html5ever`'s
size impact on the .wasm is non-trivial (~400 KB compiled).

---

## Phase E8  Files / IO (~3 days, capability-gated)

Security-sensitive. The host already supports capability
declaration on the manifest; v1 ships a single
`filesystem-readonly` capability tag and a
`filesystem-readwrite` for the symmetric case.

### fileio (~1 day)
- `readfile(path)` → BLOB
- `writefile(path, blob)` → INTEGER (bytes written)
- `lsmode(mode)`  permission-bits to symbolic, mostly for ls-like SQL queries
- `lstat_size(path)`, `lstat_mtime(path)`, `lstat_mode(path)`  scalar wrappers

`std::fs` straight through. Capability check at the dispatch
boundary  if the loaded manifest doesn't declare the
appropriate fs capability, the call errors before wasm even
runs.

### zipfile (~2 days)
Vtab:
```sql
CREATE VIRTUAL TABLE z USING zipfile('/path/to/archive.zip');
SELECT name, sz, data FROM z;
```
Columns: `name`, `mode`, `mtime`, `sz` (uncompressed),
`rawdata` (compressed bytes), `data` (decompressed),
`method`, `z` (raw zip directory record bytes), `zfile`
(filename) HIDDEN.

`zip` crate. The `data` column triggers on-demand
decompression so listings don't pay the full archive's
decode cost.

### csvtab  **EXTEND** (~half day)
The existing `extensions/csv/` is read-only with full-row
materialisation. Phase E8 follow-on: add `mode=write` which
turns vtab INSERTs into appends to the source CSV; type
inference via `csv` crate's `ReaderBuilder::has_headers` and
peek-at-first-row.

---

## Phase E9  HTTP (~4 days  most architecturally involved)

Needs WASI HTTP outgoing-handler wiring in the host. Currently
the cli imports WASI but not the http/outgoing-handler
interface; that's the gating work.

### Host side (~2 days)
- Extend the linker setup in `host/src/lib.rs` to add
  `wasmtime_wasi_http::add_to_linker_sync(...)`.
- Add `http-client` to the capability declarations the
  extension can request.
- The cli wasm picks up `wasi:http/outgoing-handler` as an
  optional import; extensions can call it.

### extensions/http (~2 days)
Scalars:
- `http_get(url)` → BLOB (response body)
- `http_get_text(url)` → TEXT
- `http_get_json(url)` → TEXT
- `http_post(url, body, content_type)` → BLOB
- `http_status_code(url)` → INTEGER (last response status; uses a per-call channel)
- `http_user_agent(name)`  scoped UA override

Vtab:
```sql
SELECT * FROM http_paginate(
  url='https://api.example.com/items',
  next_field='next',     -- JSON path to next-page URL
  items_field='data'      -- JSON path to row array
);
```
Streams pages until `next_field` is null. Useful for ingesting
paginated APIs into SQLite directly.

**Open questions**: TLS root certs in wasm32-wasip2 (wasi-http
needs SOMETHING; usually punts to host); rate limiting (probably
not in v1); HTTP/2 (also probably not v1).

---

## Phase E10  Sqlean odds-and-ends (~1 day)

### ipaddr (~half day)
Scalars:
- `ip_family(addr)` → INTEGER (4 or 6)
- `ip_in_cidr(addr, cidr)` → INTEGER
- `ip_host(addr)`, `ip_network(addr)`, `ip_broadcast(cidr)` → TEXT
- `ip_contains(cidr_a, cidr_b)` → INTEGER

`ipnet` crate.

### misc small bits (~half day)
- `eval(sql)`  the `ext/misc/eval.c` shape; overlaps with our
  `define_call` but for arbitrary SQL execution. Adds a
  capability check.
- `wholenumber()`, `nextchar()`  toy ports from `ext/misc/`.
  Out of scope unless a caller asks.

---

## Sequencing recommendation

1. **E1 (sketches)** first  no architecture changes, broadest
   reuse downstream (bloom prefilter for vec0, HLL for
   telemetry).
2. **E2 (codecs)** next  trivial mechanical wins.
3. **E3 (text)** + **E4 (stats)**  small but valuable
   per-extension.
4. **E5 (time)** + **E10 (ipaddr)**  scalar grab-bag.
5. **E6 (crypto/auth)**  security-sensitive but
   self-contained.
6. **E7 (parsing)**  bigger wasm size hit (html5ever) so
   parallel to but not blocking other phases.
7. **E8 (files)**  capability machinery first ride.
8. **E9 (HTTP)**  last, most architecturally involved.

Phases are individually shippable; the user can choose any
order at execution time. Estimates assume the existing
extension-loading + persistence + capability infrastructure
stays as-is.

---

## Out of scope (intentional)

- **In-place updates to SQLite tables from a vtab xUpdate**
  the cli's MODULE has xUpdate=None today. Adding it is its
  own plan with its own test surface (transactional
  semantics, xRollback, savepoints). The extensions in this
  plan are all read-only or use shadow-table persistence.
- **Architectural-layer changes** (cksumvfs, encryption at
  rest, alternate VFS implementations)  out of the
  "extension" category entirely.
- **Schema migrations / DDL extensions**  belongs in cli
  command surface, not extension layer.
- **Protobuf**  needs schema machinery that doesn't exist
  yet. Separate plan when a use case arrives.

---

## Total estimated effort

| Phase | Days |
|---|---:|
| E1 — sketches & graph | 5-6 |
| E2 — codecs | 2 |
| E3 — text utilities | 4 |
| E4 — statistics | 2 |
| E5 — time / date | 2 |
| E6 — crypto / auth | 3 |
| E7 — web / parsing | 2 |
| E8 — files / IO | 3 |
| E9 — HTTP | 4 |
| E10 — sqlean odds | 1 |
| **Total** | **~28-30 days** |

Single-thread; phases parallelize freely across developers.
