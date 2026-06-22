# Smoke matrix triage (post-rename pass)

Starting state: 80 failures (22 FAIL-load + 58 FAIL-mismatch).
After rename pass: 53 failures (7 FAIL-load + 47 FAIL-mismatch).
PASS count: 104 → 134.

## Remaining failures by category

### MISMATCH_VAL (39) — fixture wrong arg type or wrong expectation

Most are auto-derived fixtures passing `'test'` to functions that need INTEGER, REAL, or BLOB. Fixture pattern needs per-extension hand-rolling. Examples:

- bignum, bn_abs needs valid numeric string
- bloom_count needs BLOB filter handle
- codecs/cbor_decode needs valid CBOR bytes
- cron_next needs INTEGER timestamp
- decimal_pow2 needs INTEGER N
- h3_cell_resolution needs INTEGER cell-id
- humansize_bytes needs INTEGER, not 'test'
- ieee754_exponent needs REAL
- numfmt_commas needs numeric
- onnx_input_names needs INTEGER session-id
- phone needs (TEXT region, TEXT phone) not just one arg
- qrcode/qr_modules needs data text
- s2_cell_children needs INTEGER cell-id
- sqlean-vsv needs file path + TEXT schema arg

Subset: fixture EXPECTATION wrong despite call working:
- country: returns alpha2 code; should call country_name
- currency: returns decimals; should call currency_name or currency_symbol
- detect: lang_confidence returns score; expected lang code
- humansize: works but takes int not string
- ical/latlon/mailto/morse/ssn: various

### MISSING_FN placeholder (8)

These extensions have a `<name>_placeholder` function that's the auto-derive's pick but probably throws-by-design. Need to pick a different function from the same extension:
- aead, color-palette, image-meta, lru-cache, polyline-simplify, ssh-key, whois-parse

Plus: parsers fixture calls `hex_to_rgb` which doesn't exist in this extension (parsers is a parser-combinator crate, not color tools).

### POLICY (2) — capability-gated

- dns (needs --capability dns)
- http (needs --capability http)

Need either fixture-level capability declaration OR smoke runner --capability passthrough.

### WIT_BINDGEN (5) — extension source needs WIT update

These 5 extensions' bindings are still stale post-refactor. Need source-level fix:
- isbn, iso, lemmatize, pinyin, stemmer

(Down from the 14 the rebuild commit warned about — some made it through.)

## Recommended order

1. Hand-roll fixtures for the 8 placeholder-stuck extensions (cheap).
2. Fix the country/currency/detect-style expectation mismatches (cheap, ~10).
3. Hand-roll fixtures for arg-type mismatches (~20 lines per fixture).
4. Wire capability flags through the smoke runner (architectural).
5. Bucket-4 WIT-bindgen fixes are per-extension code work; not in scope.
