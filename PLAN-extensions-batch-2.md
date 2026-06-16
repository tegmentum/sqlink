# Plan: extension catalog batch 2

> **Phases F1 + F2 + F3 + F4 + F7 + F8 + F9 + F10 status:
> shipped.** Eight extensions delivered:
>
> * F1 geo  H3 + geohash + Maidenhead (11 scalars)
> * F2 ids  ULID + nanoid + Twitter snowflake (9 scalars)
> * F3 crypto-keys  ed25519 + x25519 + AEAD + merkle
>   (13 scalars)
> * F4 time-series  time_bucket scalar + gap_fill_series
>   eponymous TVF
> * F7 text-nlp  text_diff + markdown_to_html +
>   stem_porter + soundex + metaphone (5 scalars)
> * F8 db-utils  schema_tables / schema_columns /
>   schema_indexes / schema_to_sql / explain_query_plan
>   (6 scalars, spi-driven)
> * F9 parsers  hex/RGB/HSL color conversions + color
>   lighten/darken + length/mass/temperature unit
>   conversion + Luhn + IBAN validation (12 scalars)
> * F10 sketches  hand-rolled t-digest + MinHash with
>   in-tree wire formats (4 scalars + 2 aggregates)
>
> 31 native unit tests pass across the eight extensions.
> Pure-Rust deps only; components wrap with the
> wasi-preview1 adapter where rng is needed (ids,
> crypto-keys).
>
> **F6a + F6b status: shipped (partial F6).** Two more
> codec extensions:
>
> * formats  TOML / INI / XML+XPath-lite bundled into one
>   .wasm. Hand-rolled INI parser (~80 LOC), quick-xml
>   for XML with an in-tree minimal XPath subset
>   (absolute paths, descendant-anywhere `//`, attribute
>   selector `@`). 7 scalars; 5 native tests pass.
> * avro  Apache Avro single-record encode + decode via
>   the apache-avro crate. Schema-on-call (caller passes
>   the schema string alongside each value). 3 scalars
>   (encode + decode + version); 2 native tests pass
>   including a round-trip + a "smaller than JSON" size
>   assertion.
>
> Remaining F6 items (arrow / parquet / excel) +
> F5 + F11 deferred  individual heavy crates each
> deserve dedicated commits.
>
> **F5a status: shipped.** bpe extension wraps tiktoken-rs's
> cl100k_base encoding (the GPT-3.5/GPT-4 vocab); the model
> ~3 MB of vocab + merges  is bundled directly into the
> .wasm so callers don't ship vocab files out-of-band.
> Encoder is lazily-initialised in a thread_local on first
> call. 4 scalars: bpe_encode, bpe_decode, bpe_count_tokens,
> bpe_model_name. 3 native tests pass including the
> canonical "hello world"  [15339, 1917] vector. End-to-
> end smoke through the cli verifies the full encode 
> count  decode cycle.
>
> F5b (ONNX inference) + F5c (bundled embedding model)
> deferred. F6 remaining items (arrow / parquet / excel)
> and F11 deferred too  larger individual surfaces.

## Goal

Ship the second wave of extensions identified after the first
catalog survey closed (PLAN-extensions-followups all phases
shipped). Eleven phases grouped by domain so each phase shares
crate vocabulary and review burden.

Pre-existing infrastructure (does NOT require changes for this
batch):
- minimal / minimal-http / tabular / stateful worlds  cover all
  the export shapes we need.
- ScalarRoute routing in `dispatch_scalar` already picks the
  right cached Store based on manifest content + http
  capability.
- Capability machinery: `Capability::Http` is wired end-to-end;
  the same shape extends naturally for any new capability
  variants the data-format phases need (e.g. file
  read-only).
- Component-wrap with the wasi-preview1 adapter
  (`~/.cache/xtran/wasi_snapshot_preview1.reactor.wasm`)
  remains the standard packaging.

Each phase is independently mergeable; default sequence is by
leverage-per-day, but reordering is fine.

---

## Phase F1  Geo small wins (~3 days)

### h3 (~2 days)
Pure-Rust port of Uber's hex-grid index via `h3o` crate.
Scalars:
- `h3_to_cell(lat, lon, resolution)`  TEXT (cell index hex)
- `h3_to_geo(cell)`  TEXT (`'lat,lon'` of cell centroid)
- `h3_cell_to_boundary(cell)`  TEXT (JSON array of corners)
- `h3_resolution(cell)`  INTEGER
- `h3_neighbors(cell)`  TEXT (JSON array of neighbor cells)
- `h3_distance(a, b)`  INTEGER (hex-grid hops)
- `h3_is_pentagon(cell)`  INTEGER
- `h3_polyfill(geojson, resolution)`  TEXT (JSON array of cells)

Pair with postgis-bridge for "find all cells inside this
polygon" workflows; alternative to postgis raster for some
indexing use cases.

### geohash (~half day)
- `geohash_encode(lat, lon, precision)`  TEXT
- `geohash_decode(hash)`  TEXT (`'lat,lon'`)
- `geohash_neighbors(hash)`  TEXT (JSON array of 8 neighbor hashes)
- `geohash_bbox(hash)`  TEXT (JSON `[minlat, minlon, maxlat, maxlon]`)

Hand-rolled, no crate. ~80 LOC.

### maidenhead (~1 hr)
Ham radio grid squares. Tiny lookup-table-style encoding.
- `maidenhead_encode(lat, lon, precision)`  TEXT
- `maidenhead_decode(grid)`  TEXT

---

## Phase F2  Identity / ID generators (~1 day)

Single bundled extension `extensions/ids` exposing four ID
schemes:

- `ulid()`  TEXT (Crockford base32, time-sortable, 128-bit)
- `ulid_monotonic()`  TEXT (within same ms, strictly increasing)
- `ulid_to_timestamp(ulid)`  INTEGER (unix epoch ms)
- `nanoid([size])`  TEXT (default 21 chars)
- `nanoid_custom(alphabet, size)`  TEXT
- `snowflake([worker_id, [epoch_ms]])`  INTEGER (Twitter's
  scheme: 41-bit timestamp + 10-bit worker + 12-bit sequence)
- `snowflake_to_timestamp(id, [epoch_ms])`  INTEGER

Snowflake worker_id defaults to process pid mod 1024; the
optional epoch_ms lets users pick a custom 0-time.

`ulid` crate, `nanoid` crate. ~200 LOC total.

---

## Phase F3  Key-based crypto (~3 days)

Bundled `extensions/crypto-keys`. Pure-Rust deps from
RustCrypto.

### ed25519 (~1 day)
- `ed25519_keygen()`  BLOB (32-byte seed; private key)
- `ed25519_public(secret)`  BLOB (32-byte public key)
- `ed25519_sign(secret, msg)`  BLOB (64-byte signature)
- `ed25519_verify(public, sig, msg)`  INTEGER (0/1)

### x25519 (~half day)
- `x25519_keygen()`  BLOB
- `x25519_public(secret)`  BLOB
- `x25519_shared_secret(my_secret, their_public)`  BLOB

### AEAD ciphers (~1 day)
- `chacha20poly1305_encrypt(key, nonce, ad, plaintext)`  BLOB
- `chacha20poly1305_decrypt(key, nonce, ad, ciphertext)`  BLOB
- `aes_gcm_encrypt(key, nonce, ad, plaintext)`  BLOB
- `aes_gcm_decrypt(key, nonce, ad, ciphertext)`  BLOB

### Merkle (~half day)
- `merkle_root(leaves_blob)`  BLOB (sha-256 by default)
- `merkle_proof_verify(root, leaf, proof_blob)`  INTEGER

### secp256k1  **DEFERRED**
Used heavily by Bitcoin/Ethereum but the crate is large (~600
KB compiled). Move to its own phase once a caller asks.

---

## Phase F4  Time series helpers (~3 days)

Extend `extensions/time` or new `extensions/time-series`:

### time_bucket (~half day)
- `time_bucket(ts, '1 hour')`  TEXT (bucket-start ts)
- `time_bucket(ts, interval_secs)`  TEXT (integer-seconds form)

Matches TimescaleDB's bucketing semantics. Bucketing aligns to
the unix epoch by default; an optional 3rd `origin` arg lets
the caller bias.

### gap_fill (~1 day)
Aggregate that, given a time column and a value column, emits
rows for missing time slots between min and max. SQL surface:
```sql
SELECT time_bucket(ts, '1 hour') AS bucket,
       avg(value) FILTER (WHERE value IS NOT NULL) AS avg_v,
       last_value(value) FILTER (WHERE value IS NOT NULL) AS last_v
FROM events
WHERE ts > now() - INTERVAL '1 day'
GROUP BY bucket
-- Then gap-fill the result client-side via a wrapping query
-- using gap_fill_series(min_ts, max_ts, '1 hour') as the
-- left side of a LEFT JOIN.
```

Ship `gap_fill_series(start, end, interval)` as a TVF (similar
shape to generate_series).

### moving_avg / ema (~1 day)
Window aggregates:
- `moving_avg(value) OVER (... ROWS BETWEEN N PRECEDING AND CURRENT ROW)`
  same shape as `avg()` but defined explicitly so users don't
  have to remember the OVER clause is required.
- `ema(value, alpha)`  exponential moving average; alpha in
  (0, 1] is the smoothing factor.

Both register with `is_window=true` in the manifest. xValue /
xInverse already wired up since the cli supports window mode
on the aggregate dispatch.

### tumbling_window / sliding_window  **DEFERRED**
Streaming-style operators; SQLite's standard window functions
already cover the same surface for finite ranges, so this is
DX polish rather than a new capability.

---

## Phase F5  Vector / AI ecosystem (~2-3 weeks)

Three modules; each is its own commit. F5 is the biggest phase
in this batch.

### F5a  BPE tokenizer (~3 days)
- `bpe_load(model_path)`  INTEGER (handle)
- `bpe_encode(handle, text)`  TEXT (JSON list of token IDs)
- `bpe_decode(handle, ids_json)`  TEXT
- `bpe_vocab_size(handle)`  INTEGER

`tokenizers` crate from huggingface. Handle-based API matches
the pattern postgis-bridge's STRtree uses. The user supplies
the tokenizer.json model file via a filesystem path.

### F5b  ONNX inference (~1 week)
- `onnx_load(model_path)`  INTEGER (handle)
- `onnx_run(handle, input_json)`  TEXT (JSON output tensor)
- `onnx_input_names(handle)` / `onnx_output_names(handle)`  TEXT
- `onnx_unload(handle)`  INTEGER

`tract-onnx` crate (pure-Rust ONNX runtime). Bigger crate but
wasm32-clean. Pair with vec0 + bpe_tokenizer for the canonical
"embed text, search" workflow:
```sql
SELECT bpe_decode(tokenizer, ids) AS text,
       vec_distance_cosine(emb, ?) AS d
FROM docs JOIN ...
```

### F5c  Bundled embedding model (~1 week)
- `embed_text(s)`  BLOB (384-d f32 vector, no model handle)

Bundles a small sentence-transformers model (MiniLM-L6 ~22 MB
compressed) inside the wasm. ONE-call ergonomics: no tokenizer
load, no model load, just text  vector. Internally uses F5a +
F5b.

Worth its own commit because the model file is 22 MB; pulls
the extension's .wasm from ~2 MB to ~30 MB. The user pays
that cost ONLY if they `.load` this extension.

---

## Phase F6  Data formats (~2 weeks)

Read-only at first; write-back support is a follow-on per
format.

### F6a  arrow read (~3 days)
vtab over an Apache Arrow IPC file:
```sql
CREATE VIRTUAL TABLE t USING arrow('/path/to/data.arrow');
SELECT * FROM t LIMIT 10;
```
Schema is inferred from the file's Arrow schema. `arrow-rs`
crate; size hit ~800 KB but pulls in a lot of useful machinery.

### F6b  parquet read (~3 days)
Same vtab shape as arrow but `parquet` crate. Pair with vec0
for "embeddings sitting in a parquet file"  filter then load
into source-table-of-vec0 workflow.

### F6c  excel/xlsx read (~3 days)
`calamine` crate. vtab over a sheet.

### F6d  xml + xpath (~2 days)
Scalars:
- `xml_extract(doc, xpath)`  TEXT (concatenated text)
- `xml_extract_all(doc, xpath)`  TEXT (JSON array)
- `xml_attr(doc, xpath, attr)`  TEXT
- `xml_to_json(doc)`  TEXT (basic conversion)

`quick-xml` + a minimal XPath subset (or `sxd-xpath` for full
support; size tradeoff).

### F6e  toml + ini (~1 day combined)
- `toml_to_json(text)` / `json_to_toml(text)`
- `ini_to_json(text)` / `json_to_ini(text)`

`toml` + `serde_ini`. Mechanical.

### F6f  avro (~2 days)
`apache-avro`. `avro_decode(blob, schema)`  JSON; `avro_encode(
json, schema)`  BLOB.

---

## Phase F7  Text / NLP deeper (~2-3 days)

Bundled `extensions/text-nlp` for the smaller pieces:

- `text_diff(a, b, format)`  TEXT  unified, JSON, or HTML
  via `similar` crate
- `markdown_to_html(md)`  TEXT  `pulldown_cmark`
- `html_to_markdown(html)`  TEXT  `html2md`
- `stem_porter(word)`  TEXT  `rust-stemmers`
- `stem_snowball(word, language)`  TEXT  snowball multi-lang
- `soundex(word)`  TEXT  4-char phonetic key
- `metaphone(word)`  TEXT  double-metaphone

Pairs with spellfix1 for "phonetic-then-edit-distance" search
queries.

---

## Phase F8  Database utilities (~1 week)

### explain_parse (~1 day)
- `explain_parse(sql)`  TEXT (JSON tree)
- `explain_query_plan(sql)`  TEXT (Postgres-style tree)
Parse `EXPLAIN QUERY PLAN` output into a structured form so
users can SELECT operators / detect table-scans /
table_indexes-used etc.

### changeset_apply (~2 days)
The bundled SQLite has `SQLITE_ENABLE_SESSION`  changesets
work at the C level. Surface from SQL:
- `changeset_create(start_ts)`  BLOB (changeset binary)
- `changeset_apply(blob)`  INTEGER (rows changed)
- `changeset_invert(blob)`  BLOB (undo changeset)
- `changeset_concat(blob_a, blob_b)`  BLOB

Required for replication / undo workflows.

### schema_diff (~2 days)
- `schema_diff(db_a, db_b)`  TEXT (JSON tree of differences)
- `schema_to_sql(table_name)`  TEXT (DDL for a table)
- `schema_columns(table_name)`  TEXT (JSON list of columns)

`db_a` and `db_b` accept paths or `:memory:`. Useful for
migration validation.

### query_explain_html (~1 day)
- `explain_to_html(sql)`  TEXT
Beautified visualisation of the query plan. Pairs with the
markdown_to_html / html_to_markdown extensions for
documentation flows.

---

## Phase F9  Color + units + parsers (~1 week)

Bundled `extensions/parsers-and-converters` for the small but
ubiquitous helpers:

### Color (~1 day)
~30 scalars:
- `hex_to_rgb(hex)`, `rgb_to_hex(r, g, b)`, `hex_to_hsl(hex)`,
  `hsl_to_rgb(h, s, l)`, etc.
- `color_lighten(hex, pct)`, `color_darken(hex, pct)`
- `color_distance(a, b)`  perceptual delta-E
- `color_contrast(a, b)`  WCAG ratio

### Units (~1 day)
- `convert_length(value, from, to)`  e.g. `(1, 'm', 'ft')`
- `convert_mass(value, from, to)`
- `convert_temperature(value, from, to)`  F/C/K
- `convert_time(value, from, to)`
- `convert_volume(value, from, to)`
- `convert_pressure(value, from, to)`

Static lookup table per unit family; ~200 LOC.

### Phone (~1 day)
`phonenumber` crate:
- `phone_normalize(num, region)`  TEXT (E.164)
- `phone_format(num, region, style)`  TEXT
- `phone_country(num)`  TEXT (region code)
- `phone_validate(num, region)`  INTEGER

### Email (~1 day)
- `email_validate(addr)`  INTEGER
- `email_normalize(addr)`  TEXT (lowercase + plus-tag strip)
- `email_domain(addr)`, `email_local(addr)`  TEXT
- `email_parse_headers(raw_email)`  TEXT (JSON header map)

### Financial validation (~half day)
- `luhn_check(n)`  INTEGER (credit card / IMEI)
- `iban_validate(iban)`  INTEGER
- `iban_format(iban)`  TEXT
- `vat_validate(vat_id)`  INTEGER  per-country format
- `bic_validate(bic)`  INTEGER  SWIFT codes

---

## Phase F10  Sketches deeper (~1 week)

Beyond bloom / hll / count_min:

### t_digest (~2 days)
Better quantile estimation than count_min over numeric streams:
- `t_digest(value)`  aggregate, STATE
- `t_digest_quantile(state, q)`  REAL
- `t_digest_cdf(state, value)`  REAL

`tdigest` crate.

### hyperminhash (~2 days)
Jaccard similarity from compact sketches:
- `hmh(value)`  aggregate STATE
- `hmh_jaccard(a, b)`  REAL
- `hmh_intersection(a, b)`  INTEGER (estimated)

### datasketches family (~3-5 days)
- Theta sketches (set ops + cardinality)
- Quantiles sketches
- Frequent items
Either pure-Rust port (lots of code) or FFI to Apache
DataSketches C++ (build complexity). Defer until a specific
caller asks.

---

## Phase F11  Networking, niche (~1 week)

### dns_resolve (~1 day, capability-gated)
- `dns_resolve(name, type)`  TEXT (JSON array of records)
`hickory-resolver` crate. Same capability/policy machinery as
http  pass `--grant=dns --allowed-domains=...` at load.

### mbtiles / pmtiles (~3 days, capability-gated)
vtabs over Mapbox raster tile formats. Used heavily by
mapping pipelines. mbtiles is SQLite-backed already (just a
read wrapper); pmtiles is a single-file format with its own
parser  `pmtiles` crate.

### typst / latex math render  **DEFERRED**
`typst-cli` is large + binary-shipping; outside the
extension scope.

### whois  **DEFERRED**
Port 43 lookups. Useful but rarely from SQL.

---

## Sequencing recommendation

| Order | Phase | Why first |
|---|---|---|
| 1 | **F1 (geo small)** | Independent, broad appeal, no deps to worry about |
| 2 | **F2 (IDs)** | 1 day, used universally |
| 3 | **F3 (crypto-keys)** | Auth ecosystem completion |
| 4 | **F4 (time series)** | DX win for analytics workloads |
| 5 | **F7 (text/NLP)** | Pairs with existing text-utils/spellfix1 |
| 6 | **F8 (database utils)** | Self-referential, no new crates of size |
| 7 | **F9 (color/units/parsers)** | Mechanical mix; can parallelize |
| 8 | **F10 (sketches)** | Algorithmic; benefits stat-heavy workloads |
| 9 | **F6 (data formats)** | Heavy crates but big payoff for ML/analytics |
| 10 | **F5 (vector/AI)** | Biggest phase; saved for last so the
                          rest of the catalog ships first |
| 11 | **F11 (networking/niche)** | Capability-gated; tail risk |

Total runway: ~4-6 weeks single-threaded. Phases parallelize
freely; nothing in this batch depends on anything else in this
batch architecturally.

---

## Out of scope (intentional)

- **Multi-language stemming beyond snowball**  Stanford NLP
  / spaCy ports are out of reach for wasm32 bundles.
- **Browser DOM-style HTML mutation**  scraper is read-only;
  HTML5 mutation needs html5ever's tree-builder write side
  which doesn't have a clean wasm story.
- **GPU-accelerated anything**  no GPU in wasm32-wasip2.
- **Native code FFI to system libs**  (libpq, librdkafka)
  same architectural reason as native VFS work.
- **Authoring big-data formats** (avro/parquet write)  v1
  ships read paths only; write follows after read proves out.
- **Streaming SQL**  TimescaleDB-style continuous aggregates
  would be its own plan; v1 ships request-response patterns.

---

## Estimated total effort

| Phase | Days |
|---|---:|
| F1 geo small | 3 |
| F2 IDs | 1 |
| F3 crypto-keys | 3 |
| F4 time series | 3 |
| F5 vector/AI | 14-21 |
| F6 data formats | 14 |
| F7 text/NLP | 2-3 |
| F8 database utils | 7 |
| F9 color/units/parsers | 7 |
| F10 sketches | 7 |
| F11 networking/niche | 4-7 |
| **Total** | **~65-80 days** |

Compares with PLAN-extensions-followups' ~30 days (10 phases
shipped in this session). The size jump is mostly F5 + F6
the AI ecosystem and data-format support are the heavy hitters.

If a slimmer "v2" is preferred, drop F5 + F6 (saves ~28-35
days) and the rest comes in around 30-35 days  same scale
as the first batch.
