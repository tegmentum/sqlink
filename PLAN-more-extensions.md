# Plan: more extensions  catalog fill-ins

> **Status: drafted 2026-06-20, ready to execute in parallel.**
> Eight new SQLite extensions covering the largest remaining
> catalog gaps after the jwt/hashes-fast/ulid/nanoid/lz4/zstd/h3/s2
> round. No interdependencies  every item lands as its own
> standalone-workspace crate in `extensions/NAME/`.

## Tracks

| # | Item | Track | Size |
|---|---|---|---|
| 1 | `pwhash` | Crypto | M |
| 2 | `aead` | Crypto | M |
| 3 | `fuzzy` | Text | M |
| 4 | `stemmer` | Text | S |
| 5 | `useragent` | Web | S |
| 6 | `publicsuffix` | Web | S |
| 7 | `bibcodes` | Bibliographic | M |
| 8 | `binary-codecs` | Codecs | M |

## Cross-cut: the scaffold every item shares

Same pattern as PLAN-extensions-and-handlers.md and the existing
~60 catalog extensions:

- New crate `extensions/NAME/` as a standalone workspace
  (`[workspace]` in Cargo.toml; no shared-file edits)
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

## 1  `pwhash`  M  (Crypto)

**Goal.** Password hashing with the four modern algorithms. Pairs
with jwt + the auth wasm handler  the auth story today can verify
a signed token but has nothing to verify a user's password hash
against a stored value.

**Functions.**
```
argon2_hash(password, [params_json])   -> text (PHC string format)
argon2_verify(password, phc)           -> integer (0/1)
bcrypt_hash(password, [cost])          -> text  (cost default 12)
bcrypt_verify(password, hash)          -> integer
pbkdf2_sha256(password, salt, iter, len) -> blob
pbkdf2_sha512(password, salt, iter, len) -> blob
scrypt_hash(password, [params_json])   -> text (PHC)
scrypt_verify(password, phc)           -> integer
pwhash_version()                       -> text
```

`params_json` (Argon2 / scrypt) is an optional `{"m":..,"t":..,"p":..}`
shape; unspecified fields use OWASP-recommended defaults.

**Crates.** `argon2` 0.5, `bcrypt` 0.16, `pbkdf2` 0.12 (with the
`hmac` + `sha2` features), `scrypt` 0.11, `password-hash` 0.5 (PHC
format helper).

**Scope.** ~half day.

**Acceptance.**
- Argon2 round-trip: `argon2_verify(p, argon2_hash(p))` == 1
- Argon2 reject: `argon2_verify(wrong, argon2_hash(p))` == 0
- bcrypt round-trip + wrong-password reject, same shape
- bcrypt with explicit cost = 4 produces a hash of the right form
- PBKDF2 with RFC 6070 test vector "password" / "salt" / 4096
  iters / 32 bytes matches the published bytes
- scrypt round-trip + wrong-password reject
- PHC strings parse round-trip in their respective verify fns

---

## 2  `aead`  M  (Crypto)

**Goal.** Authenticated encryption with associated data 
column-level encrypted fields driven from SQL parameters.

**Functions.**
```
aes_gcm_encrypt(key, plaintext, nonce, [aad])     -> blob
aes_gcm_decrypt(key, ciphertext, nonce, [aad])    -> blob | NULL
chacha20_poly1305_encrypt(key, plaintext, nonce, [aad]) -> blob
chacha20_poly1305_decrypt(key, ciphertext, nonce, [aad]) -> blob | NULL
aead_random_key_256()  -> blob (32 bytes)
aead_random_nonce_96() -> blob (12 bytes, AES-GCM + ChaCha)
aead_version()         -> text
```

Key must be 32 bytes (AES-256 / ChaCha20-Poly1305). Nonce is 12
bytes for both. Output ciphertext includes the 16-byte tag at the
end (combined format). Decrypt returns NULL on any verification
failure (wrong key, tampered ciphertext, wrong nonce, bad aad) 
no error, since the SQL contract is "did this decrypt cleanly,
yes or no".

**Crates.** `aes-gcm` 0.10, `chacha20poly1305` 0.10, `rand_core`
0.6, `getrandom` 0.2 with the `wasi` feature (so random sources
flow from wasi:random/random  the WASI reactor adapter wires
this for us).

**Scope.** ~half day.

**Acceptance.**
- Round-trip both algorithms: decrypt(encrypt(p)) == p
- Tamper one byte of ciphertext  decrypt returns NULL
- Wrong key  decrypt returns NULL
- Wrong nonce  decrypt returns NULL
- Wrong aad  decrypt returns NULL
- Known test vectors:
  - ChaCha20-Poly1305: RFC 7539 §2.8.2 worked example
  - AES-GCM: NIST CAVS GCM Test Vectors (one short case)
- aead_random_key_256 produces 32 bytes; entropy across two calls
  differs

---

## 3  `fuzzy`  M  (Text)

**Goal.** String distance + phonetic codes for dedup, near-match,
typo-tolerant search. The biggest single user-visible gap in the
catalog today  sqlean's `fuzzy` is the de-facto reference and
heavily used.

**Functions.**
```
jaro(a, b)                  -> real (0..1)
jaro_winkler(a, b)          -> real (0..1)
damerau_levenshtein(a, b)   -> integer
levenshtein(a, b)           -> integer
soundex(s)                  -> text (4 chars)
metaphone(s)                -> text
double_metaphone_primary(s)   -> text
double_metaphone_secondary(s) -> text (may be empty)
caverphone(s)               -> text (6 chars)
fuzzy_version()             -> text
```

NULL inputs propagate to NULL output.

**Crates.** `strsim` 0.11 (jaro / jaro_winkler / damerau_levenshtein
/ levenshtein), `rphonetic` 3 OR `phonetic` (for soundex /
metaphone / double_metaphone / caverphone). If no single rust
crate covers all four phonetic codes, roll the missing ones by
hand from the original specs (each is ~80 lines).

**Scope.** ~half day.

**Acceptance.**
- jaro_winkler("MARTHA","MARHTA") within 0.001 of 0.961 (the
  canonical Winkler paper example)
- damerau_levenshtein("CA","ABC") == 2 (single transposition + 1
  swap)
- soundex("Robert") == "R163" (US Census 1880 reference)
- soundex("Rupert") == "R163"  same as Robert (intentional collision)
- metaphone("Pittsburgh")  documented in smoke.expected against
  whichever crate ships
- double_metaphone("Smith") returns ("SM0", "XMT") or per-crate doc
- Empty string and NULL handling explicit in smoke

---

## 4  `stemmer`  S  (Text)

**Goal.** Snowball stemmer for FTS5 / text-search rankings.
Multi-language; English is the default.

**Functions.**
```
stem(word, [lang])    -> text  (lang defaults to 'english')
stem_languages()      -> text  (comma-separated list of supported langs)
stemmer_version()     -> text
```

`lang` can be any language `rust-stemmers` supports: english,
german, french, spanish, italian, portuguese, dutch, swedish,
norwegian, danish, finnish, russian, hungarian, romanian, turkish,
arabic, greek, tamil. Unknown lang errors with a clear message.

**Crates.** `rust-stemmers` 1.2 (pure-rust Snowball impl).

**Scope.** ~3 hours.

**Acceptance.**
- stem("running")  "run"
- stem("better")  "better" (Porter2 doesn't aggressively shorten)
- stem("histories")  "histori"
- Unknown lang  error
- One non-English check: stem("laufen", "german")  "lauf"

---

## 5  `useragent`  S  (Web)

**Goal.** Parse User-Agent strings to extract browser, OS, device.
Analytics workhorse.

**Functions.**
```
ua_browser(ua)          -> text
ua_browser_version(ua)  -> text
ua_os(ua)               -> text
ua_os_version(ua)       -> text
ua_device(ua)           -> text (e.g. "iPhone" or "Other")
ua_is_bot(ua)           -> integer (0/1)
ua_parse(ua)            -> json   (everything in one row)
useragent_version()     -> text
```

NULL input  NULL output for each. Empty UA returns NULL for
browser / os / device (not error).

**Crates.** `woothee` 0.13 (pure-rust UA classifier, ~140 KB
embedded dataset). Avoid `user-agent-parser` which bundles a 1+
MB YAML rules file.

**Scope.** ~3 hours.

**Acceptance.**
- Chrome on Linux UA  ua_browser=="Chrome", ua_os=="Linux"
- Safari on iOS UA  ua_browser=="Safari", ua_os=="iPhone" /
  ua_device=="iPhone"
- googlebot UA  ua_is_bot==1
- Firefox on macOS  parses cleanly

---

## 6  `publicsuffix`  S  (Web)

**Goal.** TLD + eTLD+1 extraction from a domain. Per the Mozilla
Public Suffix List. Common need: "show me the registrable domain
of this URL".

**Functions.**
```
psl_tld(domain)        -> text  (e.g. "co.uk" for "www.example.co.uk")
psl_etld1(domain)      -> text  (e.g. "example.co.uk")
psl_is_public(domain)  -> integer
psl_subdomain(domain)  -> text  (e.g. "www"; "" if none)
publicsuffix_version() -> text  (PSL data revision + crate version)
```

NULL input  NULL output. Invalid domain (empty, all-dots, etc) 
NULL for each.

**Crates.** `publicsuffix` 2 OR `addr` 0.15 OR `psl` 2 (the
lightweight one, bundles a static PSL). `psl` is ~700 KB of
embedded list data but no async / lookup-table overhead 
preferred.

**Scope.** ~3 hours.

**Acceptance.**
- psl_tld("www.example.co.uk") == "co.uk"
- psl_etld1("www.example.co.uk") == "example.co.uk"
- psl_etld1("api.subdomain.example.com") == "example.com"
- psl_is_public("co.uk") == 1
- psl_is_public("example.com") == 0
- psl_subdomain("www.example.com") == "www"
- psl_subdomain("example.com") == ""

---

## 7  `bibcodes`  M  (Bibliographic)

**Goal.** Bibliographic identifier validation + format. Sister to
the existing aba / bic / cusip / creditcard / isin extensions.

**Functions.**
```
isbn_is_valid(s)    -> integer  (accepts ISBN-10 and ISBN-13)
isbn_normalize(s)   -> text     (canonical ISBN-13 form)
isbn_format(s)      -> text     (with hyphens, ISBN-13 form)
issn_is_valid(s)    -> integer
issn_format(s)      -> text     (NNNN-NNNX)
doi_is_valid(s)     -> integer
doi_normalize(s)    -> text     (lowercased prefix, no leading "doi:" / URL)
orcid_is_valid(s)   -> integer
orcid_format(s)     -> text     (NNNN-NNNN-NNNN-NNNX)
lccn_is_valid(s)    -> integer
lccn_normalize(s)   -> text     (per LC's "normalization" rules)
bibcodes_version()  -> text
```

NULL  NULL on every fn. Validation rejects bad checksums + bad
lengths.

**Crates.** `isbn2` 0.4 (or roll-own; ISBN check is ~30 lines).
ISSN, DOI, ORCID, LCCN checks are all simple format + checksum 
roll-own from the published specs. Reference: ISO 8254 (ISSN),
ISO 26324 (DOI), ISO 27729 (ORCID), Library of Congress LCCN
normalization rules.

**Scope.** ~half day.

**Acceptance.**
- ISBN-13 "9780306406157"  valid; checksum-flipped  invalid
- ISBN-10 "0306406152"  valid; isbn_normalize  "9780306406152"
- ISSN "0028-0836"  valid; bad checksum  invalid
- DOI "10.1038/nphys1170"  valid; missing prefix  invalid
- ORCID "0000-0002-1825-0097"  valid; format check
- LCCN "n78890351"  valid (per LoC examples)

---

## 8  `binary-codecs`  M  (Codecs)

**Goal.** MessagePack + CBOR encode/decode  modern binary
serialization codecs alongside SQLite's builtin JSON1.

**Functions.**
```
msgpack_encode(json_value)  -> blob
msgpack_decode(blob)        -> text (JSON-encoded)
cbor_encode(json_value)     -> blob
cbor_decode(blob)           -> text (JSON-encoded)
binary_codecs_version()     -> text
```

`json_value` arg accepts a JSON-shaped TEXT (parsed via serde_json
inside the component) or a primitive (int/real/text/blob/null);
the latter wraps in a single-value encoding.

NULL  NULL.

**Crates.** `rmp-serde` 1 (MessagePack), `ciborium` 0.2 (CBOR),
`serde_json` 1 (the JSON-Value pivot).

**Scope.** ~half day.

**Acceptance.**
- Round-trip: cbor_decode(cbor_encode(json('{"a":1,"b":[2,3]}')))
  parses back to the same JSON
- Round-trip msgpack: same
- Cross-impl: msgpack_encode of `{}` matches the msgpack spec
  byte exactly (`0x80` for empty map)
- CBOR: cbor_encode(1) matches `0x01`; cbor_encode("hi") matches
  `0x62 0x68 0x69` (text-string of length 2)
- Invalid blob  decode returns NULL (or error  pick one and
  document)

---

## Sequencing

Same shape as PLAN-extensions-and-handlers.md  every item is a
new standalone crate, no shared-file edits, no agent-to-agent
dependencies. Launch all 8 in parallel; expect 510 wall-clock
minutes (each item's smoke is the long pole).

The two M-sized crypto items (pwhash, aead) and the M-sized
bibcodes item are the slowest per-agent; the S items finish first
and the workflow result aggregates the whole batch.

## Risks

| Risk | Mitigation |
|---|---|
| pwhash: argon2's default params are CPU-expensive; smoke might be slow | Use cheap test-vector params in smoke; default-params test only checks the hash format, not full strength |
| fuzzy: no single crate covers all 4 phonetic codes  may need to bundle two crates or roll one | Acceptable; document chosen crate(s) in src/lib.rs |
| useragent: woothee's embedded dataset is ~140 KB  adds to component size | Acceptable; documented in the README |
| publicsuffix: `psl` crate has a 700 KB embedded list | Component is ~1 MB total; acceptable for opt-in `.load` |
| bibcodes: LCCN normalization rules are unusually fiddly | Cover the common forms; document deferred edge cases in the source |

## What this plan does NOT include (deliberate)

- Wiring any of these into the cli embed bundle  separate
  follow-up after the catalog ships
- A `crypto` extension that wraps the lower-level primitives
  (hashing, signing, encryption) under one roof  the existing
  jwt / hashes-fast / pwhash / aead split is more discoverable
- HKDF / X25519 / Ed25519-as-scalar  the jwt extension already
  uses Ed25519 internally; a standalone scalar-style export is its
  own item if a consumer asks
- BPE tokenizer / sentence embeddings  out of scope; vec0 covers
  the vector-search end of NLP

## Acceptance for the plan itself

This plan is done when:
- 8 new `extensions/NAME/` crates exist on main
- `make ext-smoke-all` is green with the new smokes included
  (53  61 total)
- Each commit references its plan item number ("feat(ext): pwhash
   ... (PLAN-more-extensions.md #1)")
