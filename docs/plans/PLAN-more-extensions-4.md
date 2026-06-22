# Plan: more extensions  round 4

> **Status: drafted 2026-06-20, ready to execute in parallel.**
> Eight more SQLite extensions  document/media depth, geo coords,
> math/signal, crypto wire formats. Every item maps to a real
> standard (ISO/RFC/IEEE) or textbook algorithm and a maintained
> pure-rust crate. All eight names pre-checked against the
> existing catalog  none collide.

## Tracks

| # | Item | Track | Size | Backing |
|---|---|---|---|---|
| 1 | `pdf-meta` | Document | M | ISO 32000 / PDF 1.7 |
| 2 | `id3` | Media | S | ID3v1 / ID3v2.3 / ID3v2.4 |
| 3 | `vcard` | Document | S | RFC 6350 |
| 4 | `mgrs` | Geo | S | DoD MGRS spec |
| 5 | `fft` | Math/Signal | M | Cooley-Tukey FFT |
| 6 | `hypothesis` | Statistics | M | textbook hypothesis tests |
| 7 | `asn1` | Crypto wire | M | ITU-T X.680/X.690 |
| 8 | `tls-cert` | Crypto wire | M | RFC 5280 (X.509) |

## Cross-cut

Same scaffold as PLAN-more-extensions{,-2,-3}.md:

- New crate `extensions/NAME/` as a STANDALONE WORKSPACE
- `.gitignore` for `target/` and `Cargo.lock`
- `src/lib.rs` with `wit_bindgen::generate!({world: "tabular"})`
- `smoke.sql` + `smoke.expected`
- Build via `make ext NAME=foo`
- Smoke executed live; `smoke_evidence` captured on report

**Pre-flight rule:** Before any code, run `ls extensions/NAME` and
`git log --oneline -- extensions/NAME 2>/dev/null | head -5`. If
the directory exists, STOP and report status=partial with the
existing surface and an "extend or rename" question. Do not blow
away or silently overwrite an existing extension.

---

## 1  `pdf-meta`  M  (Document)

**Goal.** PDF document metadata from blob: title, author, subject,
creator/producer, creation/modification dates, page count, PDF
version. `image-meta` covers raster blobs; this is the document
counterpart.

**Functions.**
```
pdf_title(blob)          -> text
pdf_author(blob)         -> text
pdf_subject(blob)        -> text
pdf_creator(blob)        -> text
pdf_producer(blob)       -> text
pdf_creation_date(blob)  -> text  (ISO 8601 if parseable)
pdf_mod_date(blob)       -> text  (ISO 8601 if parseable)
pdf_page_count(blob)     -> integer
pdf_pdf_version(blob)    -> text  (e.g. "1.7" or "2.0")
pdf_is_encrypted(blob)   -> integer
pdf_keywords(blob)       -> text  (raw keywords field; comma-sep)
pdf_all(blob)            -> text  (JSON object of every field)
pdf_meta_version()       -> text
```

NULL or non-PDF blob  NULL on each fn. Encrypted PDFs return
metadata fields they expose unencrypted (mostly /Info dictionary
is OK) and `pdf_is_encrypted == 1`.

**Crates.** `lopdf` 0.34 (pure rust; lenient parser; the
established choice for header/metadata work without full
rendering).

**Scope.** ~half day.

**Acceptance.**
- A fixture PDF with known metadata: each field extracted
  correctly
- pdf_page_count on a 3-page PDF == 3
- Random bytes  every fn returns NULL
- Encrypted-but-readable-metadata PDF (PDF 1.7 has a few common
  formats)  pdf_is_encrypted == 1, title/author still extract
- A truncated PDF (header only)  pdf_pdf_version still extracts

---

## 2  `id3`  S  (Media)

**Goal.** ID3 tag parsing from MP3 blobs. Covers ID3v1, ID3v2.3,
ID3v2.4. Audio-database workhorse.

**Functions.**
```
id3_title(blob)        -> text
id3_artist(blob)       -> text
id3_album(blob)        -> text
id3_year(blob)         -> integer
id3_genre(blob)        -> text
id3_track(blob)        -> integer  (track number)
id3_disc(blob)         -> integer
id3_comment(blob)      -> text
id3_album_artist(blob) -> text
id3_composer(blob)     -> text
id3_duration_ms(blob)  -> integer  (from MPEG frame headers if available)
id3_version(blob)      -> text  (e.g. "ID3v2.4" or "ID3v1")
id3_all(blob)          -> text  (JSON object of every frame)
id3_meta_version()     -> text
```

NULL on blobs without ID3 / random bytes. ID3v2.3 vs 2.4 frame
naming differences hidden behind the function names (e.g. both
TIT2 and TT2 map to id3_title).

**Crates.** `id3` 1.x (pure rust; covers v1 + v2.3 + v2.4; the
established choice).

**Scope.** ~3 hours.

**Acceptance.**
- A fixture MP3 with known ID3v2 tags: every field extracts
- ID3v1 (128-byte trailer): title/artist/album/year/comment
  extracted; v2-only fields  NULL
- Random bytes  every fn NULL
- id3_version reports "ID3v2.4" / "ID3v2.3" / "ID3v1" as appropriate

---

## 3  `vcard`  S  (Document)

**Goal.** vCard 2.1 / 3.0 / 4.0 contact parsing. RFC 6350.
Sister to the existing `ical` extension (calendars); vcards are
the contact-list parallel.

**Functions.**
```
vcard_fn(s)            -> text   (Full Name, FN property)
vcard_email(s)         -> text   (first EMAIL value)
vcard_emails(s)        -> text   (JSON array of all EMAIL values)
vcard_phone(s)         -> text   (first TEL value)
vcard_phones(s)        -> text   (JSON array of all TEL values)
vcard_org(s)           -> text   (ORG)
vcard_title(s)         -> text   (TITLE)
vcard_addresses(s)     -> text   (JSON array of address objects)
vcard_birthday(s)      -> text   (BDAY in ISO 8601)
vcard_url(s)           -> text   (first URL value)
vcard_note(s)          -> text   (NOTE)
vcard_version_in(s)    -> text   (vCard version, e.g. "3.0")
vcard_all(s)           -> text   (JSON object of every property)
vcard_version()        -> text   (ext version)
```

Accepts a single vCard or a multi-vcard concatenation (returns
first card's fields). NULL on parse failure or absent field.

**Crates.** `vcard4` 0.7 (pure rust; covers 2.1/3.0/4.0) OR
`vcard` 0.x (older but smaller). Pick by current maintenance.

**Scope.** ~3 hours.

**Acceptance.**
- A fixture vCard 3.0 with FN/EMAIL/TEL/ADR/BDAY: every field
  extracts
- vCard 4.0 fixture also parses (versions differ in syntax)
- Multi-EMAIL vCard  vcard_emails has all values
- Random text  every fn NULL

---

## 4  `mgrs`  S  (Geo)

**Goal.** Military Grid Reference System (MGRS) ↔ lat/lng
conversion. DoD coordinate standard used by military + emergency
services. `h3`/`s2` cover discrete global grids; MGRS is the
human-readable rectangular grid (e.g. `4Q FJ 12345 67890`).

**Functions.**
```
mgrs_from_latlng(lat, lng, [precision]) -> text
mgrs_to_latlng(mgrs)                    -> text  ("lat,lng" or JSON [lat,lng])
mgrs_grid_zone(mgrs)                    -> text  (e.g. "4Q")
mgrs_is_valid(s)                        -> integer
mgrs_precision(mgrs)                    -> integer (1..=5; meters of precision)
mgrs_version()                          -> text
```

`precision` 0..=5 picks the easting/northing digit count
(precision 5  1-meter accuracy, precision 0  the 100 km
grid square). NULL  NULL on each.

**Crates.** `mgrs` 0.2 OR roll-own from `utm` 0.2 (the
conversion math is tractable). MGRS is built on UTM.

**Scope.** ~3 hours.

**Acceptance.**
- Known reference: Eiffel Tower at (48.8584, 2.2945)
  mgrs_from_latlng(48.8584, 2.2945, 5) ~ '31U DQ 48251 11553'
  (UTM zone 31U, 100km square DQ; check first chars + zone)
- Round-trip: lat/lng  mgrs  lat/lng within 1m at precision 5
- mgrs_is_valid('4Q FJ 12345 67890') == 1
- mgrs_is_valid('not mgrs') == 0
- mgrs_precision('4Q FJ 12345 67890') == 5

---

## 5  `fft`  M  (Math / Signal)

**Goal.** Fast Fourier Transform  forward + inverse, real + complex
input. The signal-processing workhorse. Combined with the existing
`time-series` ext, you can do frequency-domain queries from SQL.

**Functions.**
```
fft_forward(samples_json)         -> text  (JSON [[re, im], ...] complex output)
fft_forward_real(samples_json)    -> text  (JSON real-valued input  complex output)
fft_inverse(spectrum_json)        -> text  (JSON real-valued time-domain output)
fft_magnitude(spectrum_json)      -> text  (JSON array of magnitudes)
fft_phase(spectrum_json)          -> text  (JSON array of phases in radians)
fft_power_spectrum(samples_json)  -> text  (JSON array of power values)
fft_window(samples_json, kind)    -> text  (Apply window: hann/hamming/blackman/rect)
fft_version()                     -> text
```

Inputs are JSON arrays (string TEXT). Power-of-2 lengths are
fastest; arbitrary lengths supported (rustfft picks mixed-radix
or Bluestein automatically).

**Crates.** `rustfft` 6.x (pure rust, no_std-friendly, fast).
Plus `apodize` 1 or hand-rolled window functions.

**Scope.** ~half day.

**Acceptance.**
- DC input: fft_forward('[1,1,1,1]') has [4,0] as first bin,
  zeros elsewhere
- Sin wave at f=1 of length-8 signal  peak at bin 1 (and N-1)
- Round-trip: fft_inverse(fft_forward(x)) == x within 1e-10
- fft_magnitude returns Re² + Im² square roots
- Hann window applied + then summed sums to N/2

---

## 6  `hypothesis`  M  (Statistics)

**Goal.** Statistical hypothesis tests: t-test (one-sample,
two-sample, paired), chi-squared (goodness-of-fit, independence),
ANOVA F-test, Mann-Whitney U, Kolmogorov-Smirnov. The
`dist` ext gave you distributions; this is the test surface that
uses them.

**Functions.**
```
t_test_1samp(samples_json, mu0)             -> text  (JSON {t, df, p})
t_test_2samp(a_json, b_json, [equal_var])   -> text  (JSON {t, df, p})
t_test_paired(a_json, b_json)               -> text  (JSON {t, df, p})
chi_sq_gof(observed_json, expected_json)    -> text  (JSON {chi2, df, p})
chi_sq_independence(table_json)             -> text  (JSON {chi2, df, p}; table is JSON 2D)
anova_f(groups_json)                        -> text  (JSON {F, df_between, df_within, p}; groups is JSON array-of-arrays)
mann_whitney(a_json, b_json)                -> text  (JSON {U, p})
ks_2samp(a_json, b_json)                    -> text  (JSON {D, p})
shapiro_wilk(samples_json)                  -> text  (JSON {W, p}; small samples only)
hypothesis_version()                        -> text
```

Returns JSON object so SQL consumers can `json_extract(result,
'$.p')` for the p-value. All p-values two-sided unless documented
otherwise.

**Crates.** `statrs` 0.17 (already proven to cross-compile;
provides the inverse CDF needed for p-values). Test math is
textbook  hand-roll the test statistics, defer the CDF lookups
to statrs.

**Scope.** ~half day.

**Acceptance.**
- One-sample t-test: t_test_1samp('[5.1,5.2,5.0,4.9,5.0]', 5.0)
  has small t, large p (sample mean ~ 5.04)
- Two-sample t-test on identical distributions has p ~ 0.5
- Chi-squared goodness-of-fit with [10,10,10,10] vs uniform
  expected has p high
- ANOVA on three identical samples has F close to 0, p high
- Documented tolerances in smoke.expected (3 decimal places)

---

## 7  `asn1`  M  (Crypto wire)

**Goal.** ASN.1 / DER encode + decode. The wire format underneath
X.509 certs, PKCS keys, S/MIME, LDAP, SNMP. Pairs with the
soon-to-land `tls-cert` (item 8) which decodes specific DER
structures, and with the existing `jwt` extension (RS256 keys
are PKCS#1 / PKCS#8 = DER inside).

**Functions.**
```
asn1_decode(der_blob)            -> text  (JSON tree representation)
asn1_encode(json_tree)           -> blob  (DER-encoded)
asn1_oid_name(oid_dotted)        -> text  (e.g. '1.2.840.113549.1.1.11'  'sha256WithRSAEncryption')
asn1_oid_for(name)               -> text  (reverse lookup)
asn1_is_valid_der(blob)          -> integer
asn1_type_tag(der_blob)          -> integer  (first byte's class+P/C+tag)
asn1_pretty(der_blob)            -> text  (multi-line indented JSON, debug-friendly)
asn1_version()                   -> text
```

JSON tree shape: each ASN.1 node is `{type: 'SEQUENCE'|'INTEGER'|...,
value: <type-specific>, tag?: <number>}` where SEQUENCE/SET have
a `children` array and primitive types have a `value` field.

**Crates.** `simple_asn1` 0.6 (pure rust; flexible decoder) OR
`asn1` 0.21 (DER encode + decode with explicit primitives).
Plus a curated OID  name table (~80 entries for common crypto OIDs).

**Scope.** ~half day.

**Acceptance.**
- Round-trip simple SEQUENCE { INTEGER 1, INTEGER 2 } via
  asn1_encode(asn1_decode(blob)) == blob
- asn1_oid_name('1.2.840.113549.1.1.11') ==
  'sha256WithRSAEncryption'
- asn1_oid_name('2.5.4.3') == 'commonName' (CN)
- asn1_is_valid_der of random bytes == 0
- asn1_pretty produces multi-line, indentation visible

---

## 8  `tls-cert`  M  (Crypto wire)

**Goal.** X.509 v3 certificate parsing  subject, issuer, serial,
validity, public key, SANs, signature algorithm. RFC 5280. Pairs
with `jwt` (RS256/ES256 keys come from certs) and the `useragent`
/ `publicsuffix` / `idna` family for end-to-end TLS-aware SQL.

**Functions.**
```
cert_subject(pem_or_der)          -> text   (RDN string, e.g. 'CN=example.com,O=Example Inc,...')
cert_issuer(pem_or_der)           -> text   (RDN string)
cert_serial(pem_or_der)           -> text   (hex, no separators)
cert_not_before(pem_or_der)       -> text   (ISO 8601)
cert_not_after(pem_or_der)        -> text   (ISO 8601)
cert_sig_algorithm(pem_or_der)    -> text   (e.g. 'sha256WithRSAEncryption')
cert_public_key_algorithm(pem_or_der) -> text  (e.g. 'rsaEncryption', 'ecPublicKey')
cert_public_key_bits(pem_or_der)  -> integer (RSA modulus bits / EC curve order bits)
cert_sans(pem_or_der)             -> text   (JSON array of SAN values; DNS / IP / URI / email)
cert_fingerprint_sha256(pem_or_der) -> text (hex)
cert_is_valid_now(pem_or_der)     -> integer (0/1; check NotBefore..NotAfter)
cert_self_signed(pem_or_der)      -> integer (subject == issuer)
cert_all(pem_or_der)              -> text   (JSON of every field above)
tls_cert_version()                -> text
```

Accepts PEM (with -----BEGIN CERTIFICATE----- header) or raw DER.
NULL on parse failure.

**Crates.** `x509-parser` 0.16 (pure rust; the established
choice). `pem` 3 for PEM decode.

**Scope.** ~half day.

**Acceptance.**
- A well-known cert (e.g. let's encrypt root, or a self-signed
  fixture generated in the smoke setup): every field extracts
- cert_self_signed on a known self-signed cert == 1
- cert_sans for a multi-SAN cert  JSON array of values
- cert_fingerprint_sha256 matches `openssl x509 -fingerprint -sha256`
  hex output (byte-exact, lowercased)
- cert_is_valid_now == 1 for the let's encrypt root (expires
  2035-ish); == 0 for an expired fixture

---

## Sequencing

Launch all 8 in parallel; ~510 wall-clock minutes. Same shape
as the prior three rounds.

## Risks

| Risk | Mitigation |
|---|---|
| pdf-meta: `lopdf` parses many PDF variants but not all (esp. linearized) | Document supported PDF versions in src |
| id3: ID3v2.2 (old) not supported by the `id3` crate | Document; v2.3 + v2.4 cover post-1999 files |
| vcard: vCard 2.1 has many vendor extensions | Cover RFC 2426 (v3.0) + RFC 6350 (v4.0) cleanly; 2.1 best-effort |
| mgrs: UTM polar regions use UPS  document the lat/lng limit |  Document |80°| latitude limit |
| fft: rustfft uses dynamic dispatch; small overhead | Acceptable; rustfft is the de-facto choice |
| hypothesis: p-value approximations diverge from R / scipy in tail extremes | Document tolerance in smoke; agree to use scipy as reference |
| asn1: DER vs BER vs CER distinctions | Stick to DER only (the canonical encoding); document |
| tls-cert: GeneralizedTime vs UTCTime parsing | Both handled by x509-parser; document timezone behavior |

## What this plan does NOT include (deliberate)

- EPUB / DJVU / DOCX metadata  one-format-per-ext if asked
- Audio decode (PCM / MP3 frames)  too heavy for v1
- vCard write  parse-only first; write is a separate item
- MGRS polar (UPS)  separate item; rarely needed for non-military
  consumers
- Statistical bootstrap / permutation tests  separate item
- ASN.1 BER / CER  DER only
- Certificate chain validation  parsing only; chain math is its
  own ~1 week item

## Acceptance for the plan itself

- 8 new `extensions/NAME/` crates exist on main
- `make ext-smoke-all` is green (75  83 total)
- Each commit references its plan item number
