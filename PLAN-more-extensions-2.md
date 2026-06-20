# Plan: more extensions  round 2

> **Status: drafted 2026-06-20, ready to execute in parallel.**
> Eight more SQLite extensions covering the largest remaining
> catalog gaps after the pwhash/aead/fuzzy/stemmer/useragent/
> publicsuffix/bibcodes/binary-codecs round. No interdependencies;
> every item lands as its own standalone-workspace crate.

## Tracks

| # | Item | Track | Size |
|---|---|---|---|
| 1 | `totp` | Crypto / Auth | M |
| 2 | `blake3` | Hashes | S |
| 3 | `chrono` | Time | M |
| 4 | `unicode` | Text | M |
| 5 | `toml` | Codecs | S |
| 6 | `roaring` | Data Structures | M |
| 7 | `image-meta` | Media | S |
| 8 | `mac-oui` | Network | S |

## Cross-cut: the scaffold every item shares

Same pattern as PLAN-more-extensions.md and the existing 70+
catalog extensions:

- New crate `extensions/NAME/` as a STANDALONE WORKSPACE
  (`[workspace]` empty body in Cargo.toml; no shared-file edits)
- `Cargo.toml`  cdylib crate-type, wit-bindgen + wit-bindgen-rt
  deps, item-specific algorithm crates
- `.gitignore` for `target/` and `Cargo.lock`
- `src/lib.rs` with `wit_bindgen::generate!({world: "tabular"})`
  and a Guest impl emitting `metadata::Manifest` + the scalars
- `smoke.sql` + `smoke.expected` so `make ext-smoke-all` catches
  regressions
- Build via `make ext NAME=foo` (cargo + wasm-tools component
  new + wasi-p1 reactor adapter)
- Smoke executed live; `smoke_evidence` captured on report

No agent modifies any shared file outside their crate directory.
Cli embed-bundle wiring is deferred per the established pattern.

---

## 1  `totp`  M  (Crypto / Auth)

**Goal.** TOTP (RFC 6238) + HOTP (RFC 4226) generation and
verification. The second factor in a 2FA flow  pairs with jwt
(first factor) + pwhash (password) + aead (vault encryption) to
complete the in-catalog auth surface.

**Functions.**
```
totp_generate(secret_b32, [period_s], [digits], [algorithm]) -> text
totp_verify(code, secret_b32, [period_s], [digits], [algorithm], [window]) -> integer (0/1)
hotp_generate(secret_b32, counter, [digits], [algorithm]) -> text
hotp_verify(code, secret_b32, counter, [digits], [algorithm]) -> integer
totp_url(label, secret_b32, [issuer], [period_s], [digits], [algorithm]) -> text (otpauth:// URI)
totp_secret([byte_len]) -> text (base32, default 20 bytes / 160 bits)
totp_now() -> integer (current epoch seconds; useful for testing windows)
totp_version() -> text
```

Defaults: `period_s=30`, `digits=6`, `algorithm='SHA1'` (the RFC
6238 baseline; most authenticator apps assume these), `window=1`
(accept code from ±1 step).

**Crates.** `totp-rs` 5 OR roll-own  HMAC-SHA1/256/512 plus the
RFC 4226 dynamic-truncation step is ~40 lines. The crate is
preferable; the otpauth:// URL builder is also there.

**Scope.** ~half day.

**Acceptance.**
- RFC 6238 Appendix B test vectors: at T=59 (counter 0x1)
  totp_generate('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 30, 8, 'SHA1')
  == '94287082' (after sliding the time forward via totp_url
  testing helper or a counter-based test).
- HOTP RFC 4226 Appendix D: hotp_generate(secret_b32='GEZDGNBVGY3TQOJQ',
  counter=0, digits=6, alg='SHA1') == '755224' (the published vector).
- Round-trip: code = totp_generate(secret, now);
  totp_verify(code, secret) == 1.
- Wrong code: totp_verify('000000', secret) == 0.
- otpauth:// URL form matches the Google Authenticator key URI spec.
- totp_secret(20)  base32 string of expected length (32 chars).

---

## 2  `blake3`  S  (Hashes)

**Goal.** Modern hash function  faster than SHA3, native
parallelism, keyed-hash and KDF modes built in. Slots alongside
sha3 + hashes-fast as a third hash family.

**Functions.**
```
blake3_hash(value, [output_len])    -> blob   (default 32 bytes)
blake3_hex(value, [output_len])     -> text   (lowercase hex)
blake3_keyed(key, value, [out_len]) -> blob   (key MUST be 32 bytes)
blake3_keyed_hex(key, value, [out_len]) -> text
blake3_derive_key(context_str, key_material) -> blob (32 bytes; KDF mode)
blake3_version() -> text
```

Value coercion mirrors the hashes-fast pattern (TEXTutf8, BLOB
as-is, INTEGER/REALTEXT repr, NULLempty). Output length range
1..=64 KiB (BLAKE3 is XOF; we cap to a sane SQL value).

**Crates.** `blake3` 1 (pure rust by default; SIMD optimized when
the target supports it  wasm32-wasip2 builds work clean).

**Scope.** ~3 hours.

**Acceptance.**
- Known vector: blake3_hex('') ==
  'af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262'
  (the empty-string digest per the BLAKE3 reference impl).
- blake3_hex('abc')  the published 32-byte digest.
- blake3_keyed with a 32-byte key + 'BLAKE3' as input matches the
  reference output for keyed mode (one test vector from blake3
  reference).
- blake3_hash(NULL) == blake3_hash('').
- BLOB('abc') and TEXT('abc') produce the same digest.

---

## 3  `chrono`  M  (Time)

**Goal.** Date/time arithmetic the SQLite builtin doesn't cover
timezone conversion, business-day math, ISO 8601 round-trip,
duration parsing/formatting. Cron extension already handles cron
expressions; this is the "datetime() and friends, but powered."

**Functions.**
```
date_parse(s, [format])           -> text (canonical ISO 8601 UTC)
date_format(s, format)            -> text
date_add(s, amount, unit)         -> text  (unit: years/months/days/hours/mins/secs)
date_diff(a, b, unit)             -> integer
date_tz_convert(s, from_tz, to_tz) -> text
date_now_tz(tz)                   -> text (current time in tz, ISO 8601)
date_is_business_day(s)           -> integer (Mon-Fri => 1; no holiday list in v1)
date_business_days_between(a, b)  -> integer
date_iso_week(s)                  -> integer (ISO 8601 week 1..53)
date_iso_year(s)                  -> integer
duration_parse(s)                 -> integer (seconds)
duration_format(seconds, [precision]) -> text ("1d 3h 2m" / "1.5h")
chrono_version()                  -> text
```

Format string follows chrono's strftime spec; document the
common codes (%Y, %m, %d, %H, %M, %S, %Z, %z, %FT%TZ).

**Crates.** `chrono` 0.4, `chrono-tz` 0.10 (bundles IANA tz
database; ~200 KB; acceptable for opt-in `.load`).

**Scope.** ~half day.

**Acceptance.**
- date_parse('2025-06-20T15:30:00Z')  ISO 8601 round-trip
- date_format('2025-06-20T15:30:00Z', '%Y/%m/%d') == '2025/06/20'
- date_add('2025-06-20', 5, 'days') == '2025-06-25T00:00:00Z'
- date_diff('2025-06-25', '2025-06-20', 'days') == 5
- date_tz_convert('2025-06-20T12:00:00Z', 'UTC', 'America/New_York')
  contains 08:00:00 (UTC-4 EDT)
- date_iso_week('2024-01-01') == 1
- date_is_business_day('2025-06-21') == 0 (Saturday)
- duration_parse('1d 3h')  == 97200 seconds
- duration_format(3600) == '1h'

---

## 4  `unicode`  M  (Text)

**Goal.** Unicode normalization, case folding, accent stripping,
URL slugification. Fills the Unicode-correctness gap  text-utils
covers ASCII string ops; this covers everything else.

**Functions.**
```
unicode_nfc(s)            -> text  (canonical composition)
unicode_nfd(s)            -> text  (canonical decomposition)
unicode_nfkc(s)           -> text  (compatibility composition)
unicode_nfkd(s)           -> text  (compatibility decomposition)
unicode_fold(s)           -> text  (case folding per UCA)
unicode_strip_accents(s)  -> text  (NFD + drop combining marks)
unicode_slugify(s)        -> text  (URL-safe slug, ASCII-only)
unicode_normalize_whitespace(s) -> text (collapse all whitespace runs to one space)
unicode_category(s)       -> text  (Unicode category of first char: "Lu","Ll","Nd",etc)
unicode_grapheme_count(s) -> integer
unicode_version()         -> text  (Unicode standard version + crate version)
```

NULL  NULL on each.

**Crates.** `unicode-normalization` 0.1, `unicode-casefold` 0.1
OR `unicase` 2 (for fold), `deunicode` 1 (for slugify / strip
accents), `unicode-segmentation` 1 (grapheme count).

**Scope.** ~half day.

**Acceptance.**
- unicode_nfc('e\u{0301}') == 'é' (combined  precomposed)
- unicode_nfd('é') has length 2 codepoints (decomposed)
- unicode_nfc(unicode_nfd(s)) == s for ASCII s
- unicode_fold('Straße') == 'strasse' (sharp s folds to ss)
- unicode_strip_accents('café') == 'cafe'
- unicode_slugify('Hello, World!') == 'hello-world'
- unicode_slugify('café é à') == 'cafe-e-a'
- unicode_grapheme_count('🇺🇸') == 1 (flag emoji is one grapheme)
- unicode_grapheme_count('é') == 1 even after NFD

---

## 5  `toml`  S  (Codecs)

**Goal.** TOML  JSON. Round-trip config files through SQL.
Sister to binary-codecs (msgpack/cbor); json1 covers the JSON
direction.

**Functions.**
```
toml_to_json(s)                  -> text (JSON-encoded)
json_to_toml(json_text)          -> text (TOML)
toml_get(s, key_path)            -> text (extract a single value as JSON-encoded)
toml_keys(s, [key_path])         -> text (JSON array of top-level or nested keys)
toml_is_valid(s)                 -> integer (0/1)
toml_version()                   -> text
```

`key_path` is dotted (`server.port`); supports the standard
TOML nesting. Returns NULL on missing key.

**Crates.** `toml` 0.8, `serde_json` 1.

**Scope.** ~3 hours.

**Acceptance.**
- Round-trip: t = `[server]\nport = 8080\nhost = "localhost"\n`
  toml_to_json(t)  '{"server":{"port":8080,"host":"localhost"}}'
  json_to_toml(that)  syntactically valid TOML that re-parses
- toml_get(t, 'server.port') == '8080'
- toml_keys(t) == '["server"]'
- toml_keys(t, 'server') == '["port","host"]'
- toml_is_valid('not toml [[[ ') == 0
- toml_is_valid('a = 1') == 1

---

## 6  `roaring`  M  (Data Structures)

**Goal.** Roaring bitmaps  exact-set membership at scale, with
O(1) cardinality, fast union/intersection/difference. The
catalog has probabilistic set structures (bloom, hyperloglog,
count-min); roaring is the exact-set partner you want when the
set is sparse-but-not-too-sparse.

**Functions.**
```
rb_new()                   -> blob
rb_from_array(json_array)  -> blob
rb_from_range(lo, hi)      -> blob (inclusive range)
rb_to_array(rb)            -> text (JSON array)
rb_cardinality(rb)         -> integer
rb_contains(rb, value)     -> integer
rb_add(rb, value)          -> blob
rb_remove(rb, value)       -> blob
rb_union(a, b)             -> blob
rb_intersection(a, b)      -> blob
rb_difference(a, b)        -> blob
rb_symmetric_difference(a, b) -> blob
rb_serialize(rb)           -> blob (portable Roaring spec format)
rb_deserialize(blob)       -> blob (validates + canonicalizes)
rb_version()               -> text
```

Values are u32 (the Roaring spec). i64 inputs that fit get
coerced; out-of-range inputs error.

**Crates.** `roaring` 0.10 (pure rust, no_std-friendly,
implements the portable serialization spec).

**Scope.** ~half day.

**Acceptance.**
- rb_cardinality(rb_from_array('[1,2,3]')) == 3
- rb_contains(rb_from_array('[1,2,3]'), 2) == 1
- rb_contains(rb_from_array('[1,2,3]'), 4) == 0
- rb_cardinality(rb_union(rb_from_array('[1,2]'), rb_from_array('[2,3]'))) == 3
- rb_cardinality(rb_intersection(rb_from_array('[1,2]'), rb_from_array('[2,3]'))) == 1
- rb_to_array(rb_from_range(1, 5)) == '[1,2,3,4,5]'
- Round-trip: rb_deserialize(rb_serialize(rb)) preserves contents

---

## 7  `image-meta`  S  (Media)

**Goal.** Image dimensions + format + color type from blob headers
no full pixel decode. Fits the "store image blobs, query their
metadata" use case  photo databases, image-aware vtab paths,
etc.

**Functions.**
```
img_format(blob)      -> text  ('PNG' | 'JPEG' | 'GIF' | 'WebP' | 'BMP' | 'TIFF' | 'AVIF' | 'HEIC')
img_width(blob)       -> integer
img_height(blob)      -> integer
img_dimensions(blob)  -> text  (JSON: {width, height, format})
img_byte_size(blob)   -> integer (length(blob); convenience)
img_version()         -> text
```

NULL or unrecognized blob  NULL on each. Reads ONLY the header,
not full pixel data; works on partial reads (e.g. the first 4 KB
of a TIFF).

**Crates.** `imagesize` 0.13 (pure rust, header-only decoder for
all major formats; tiny dep, no allocation in the hot path).

**Scope.** ~3 hours.

**Acceptance.**
- A 4-byte PNG signature (89 50 4E 47) followed by IHDR  width
  + height extracted correctly
- A JPEG SOI + APP0  format == 'JPEG'; SOF0  width + height
- 0xFFD8 alone (truncated JPEG)  format == 'JPEG', dimensions NULL
- Random bytes  all fns return NULL
- img_dimensions returns a JSON object parseable by json_extract

---

## 8  `mac-oui`  S  (Network)

**Goal.** MAC address parsing + IEEE OUI vendor lookup. Common
need for any network-analytics SQL workload.

**Functions.**
```
mac_is_valid(s)         -> integer
mac_normalize(s)        -> text  (lowercase, colon-separated, 6 octets)
mac_format(s, [style])  -> text  (style: 'colon' default | 'dash' | 'dot' | 'bare')
mac_oui(s)              -> text  (first 3 octets uppercased, no separators: "AABBCC")
mac_vendor(s)           -> text  (vendor name from IEEE OUI db; NULL if unknown)
mac_is_unicast(s)       -> integer (bit 0 of first octet == 0)
mac_is_universal(s)     -> integer (bit 1 of first octet == 0)
mac_random()            -> text  (random LAA address)
mac_oui_version()       -> text  (OUI list revision date + crate version)
```

Accepts input in any common format: `aa:bb:cc:dd:ee:ff`,
`aa-bb-cc-dd-ee-ff`, `aabbcc.ddeeff`, `aabbccddeeff` (any
combination of separators allowed by the standard).

**Crates.** `eui48` 1 (parsing + format) + `mac_oui` 0.5 or a
bundled MA-L CSV from IEEE for vendor lookup. The OUI list is
~30K vendors; ~1 MB embedded  acceptable for opt-in `.load`.

**Scope.** ~3 hours.

**Acceptance.**
- mac_is_valid('aa:bb:cc:dd:ee:ff') == 1
- mac_is_valid('not a mac') == 0
- mac_normalize('AA-BB-CC-DD-EE-FF') == 'aa:bb:cc:dd:ee:ff'
- mac_format('aabbccddeeff', 'dash') == 'AA-BB-CC-DD-EE-FF'
- mac_oui('aa:bb:cc:dd:ee:ff') == 'AABBCC'
- mac_vendor('00:00:0C:dd:ee:ff') == 'Cisco Systems, Inc' (well-known
  OUI)
- mac_is_unicast('aa:bb:cc:dd:ee:ff') == 0 (bit 0 of 0xAA is 0  wait, 0xAA = 10101010, bit 0 = 0  unicast = 1)
- mac_random() returns a valid LAA MAC (bit 1 of first octet == 1)

---

## Sequencing

Same shape as the prior batches  every item is a standalone
crate, no shared-file edits, no agent-to-agent dependencies.
Launch all 8 in parallel; expect 510 wall-clock minutes (each
item's smoke is the long pole).

## Risks

| Risk | Mitigation |
|---|---|
| chrono-tz embedded IANA db is ~200 KB | Acceptable; alternative is no timezone support  |
| mac-oui IEEE list is ~1 MB | Acceptable; document size in README |
| roaring serialization format is large for small bitmaps | Document that rb_serialize emits the portable spec format, not the smallest possible encoding |
| imagesize HEIF/AVIF support is partial | Document which formats are fully supported in src |
| totp HOTP test vectors need a known counter sequence | Use the RFC 4226 Appendix D vectors; document in smoke |

## What this plan does NOT include (deliberate)

- HKDF / X25519 / Ed25519 standalone scalars  the jwt extension
  uses Ed25519 internally; standalone exports are their own
  follow-up if a consumer asks
- HTTP signature / OAuth PKCE / WebAuthn  these are wasm-handler
  patterns more than scalars
- Holiday calendars for date_is_business_day  v1 is Mon-Fri;
  per-region holidays need a calendar source + regional flag
- secp256k1 (Bitcoin curve)  cryptocurrency-specific, separate
  ask
- XML / XPath  too many design choices (which dialect, namespace
  handling); separate plan
- PDF / EPUB / EXIF metadata extraction beyond image dimensions
  each format gets its own ext

## Acceptance for the plan itself

- 8 new `extensions/NAME/` crates exist on main
- `make ext-smoke-all` is green with the new smokes included
  (61  69 total)
- Each commit references its plan item number ("feat(ext): totp
   ... (PLAN-more-extensions-2.md #1)")
