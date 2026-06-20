# Plan: more extensions  round 3

> **Status: drafted 2026-06-20, ready to execute in parallel.**
> Eight more SQLite extensions  international data, document &
> media depth, statistics, language detection. Every item maps
> to a real standard (ISO / RFC / W3C) or textbook math, and to
> a maintained pure-rust crate. No interdependencies; standalone
> workspaces.

## Tracks

| # | Item | Track | Size | Backing |
|---|---|---|---|---|
| 1 | `iban` | International | S | ISO 13616 |
| 2 | `iso-codes` | International | M | ISO 3166 / 4217 / 639 |
| 3 | `html` | Document | M | HTML5 spec (html5ever) |
| 4 | `exif` | Media | M | JEITA EXIF |
| 5 | `color` | Media | M | CSS color + textbook conversions |
| 6 | `qrcode` | Media | S | ISO/IEC 18004 |
| 7 | `dist` | Statistics | M | textbook distributions |
| 8 | `lang-detect` | Text | S | n-gram heuristic |

## Cross-cut

Same scaffold as PLAN-more-extensions.md / PLAN-more-extensions-2.md.

- New crate `extensions/NAME/` as a STANDALONE WORKSPACE
- `.gitignore` for `target/` and `Cargo.lock`
- `src/lib.rs` with `wit_bindgen::generate!({world: "tabular"})`
- `smoke.sql` + `smoke.expected`
- Build via `make ext NAME=foo`
- Smoke executed live; `smoke_evidence` captured on report

---

## 1  `iban`  S  (International)

**Goal.** IBAN validation + decomposition. ISO 13616. Sister to
the existing `bic` extension; commonly needed alongside BIC for
financial workflows.

**Functions.**
```
iban_is_valid(s)         -> integer
iban_normalize(s)        -> text   (uppercase, no spaces)
iban_format(s)           -> text   (groups of 4 with spaces)
iban_country(s)          -> text   (ISO 3166-1 alpha-2, e.g. 'DE')
iban_check_digits(s)     -> text   (2 chars after country)
iban_bban(s)             -> text   (Basic Bank Account Number portion)
iban_bank_code(s)        -> text   (per-country bank code; NULL if no spec)
iban_account_number(s)   -> text   (per-country account; NULL if no spec)
iban_version()           -> text
```

NULL  NULL on every fn. Mod-97 check per ISO 7064.

**Crates.** `iban_validate` 5 (pure-rust; bundles per-country
BBAN structure rules).

**Scope.** ~3 hours.

**Acceptance.**
- iban_is_valid('DE89370400440532013000') == 1 (Deutsche Bank
  example from Wikipedia)
- iban_is_valid('GB82WEST12345698765432') == 1
- Flip one digit  iban_is_valid == 0
- iban_country('DE89370400440532013000') == 'DE'
- iban_format('DE89370400440532013000') == 'DE89 3704 0044 0532 0130 00'
- iban_normalize('de89 3704 0044 0532 0130 00') ==
  'DE89370400440532013000'

---

## 2  `iso-codes`  M  (International)

**Goal.** ISO 3166 country codes (249), ISO 4217 currency codes
(~180), ISO 639 language codes (~180). i18n workhorse.

**Functions.**
```
iso3166_alpha2_name(code)        -> text   (e.g. 'US'  'United States')
iso3166_alpha3_name(code)        -> text   (e.g. 'USA'  'United States')
iso3166_alpha2_to_alpha3(code)   -> text
iso3166_alpha3_to_alpha2(code)   -> text
iso3166_numeric(code)            -> integer (e.g. 'US'  840)
iso3166_is_valid(code)           -> integer
iso4217_name(code)               -> text   (e.g. 'USD'  'US Dollar')
iso4217_symbol(code)             -> text   (e.g. 'USD'  '$')
iso4217_minor_units(code)        -> integer (decimal places; e.g. JPY  0)
iso4217_is_valid(code)           -> integer
iso639_alpha2_name(code)         -> text   (e.g. 'en'  'English')
iso639_alpha3_name(code)         -> text   (e.g. 'eng'  'English')
iso639_alpha2_to_alpha3(code)    -> text
iso639_alpha3_to_alpha2(code)    -> text
iso639_is_valid(code)            -> integer
iso_codes_version()              -> text
```

Case-insensitive on lookup; canonical case on output (alpha-2 in
upper, alpha-3 in upper, language in lower per ISO 639). NULL 
NULL on each. Unknown code  NULL (not error).

**Crates.** `rust_iso3166` 1 (country) + `rusty-money` (currency
metadata) + `isolang` (ISO 639) OR bundle the three lists as
const slices and hand-roll the lookups (~600 lines for all
three; small and stable).

**Scope.** ~half day.

**Acceptance.**
- iso3166_alpha2_name('US') == 'United States'
- iso3166_numeric('US') == 840
- iso3166_alpha2_to_alpha3('US') == 'USA'
- iso4217_minor_units('USD') == 2
- iso4217_minor_units('JPY') == 0
- iso4217_minor_units('BHD') == 3 (Bahraini Dinar)
- iso639_alpha2_name('en') == 'English'
- iso639_alpha3_to_alpha2('eng') == 'en'
- iso3166_is_valid('XX') == 0; iso4217_is_valid('XYZ') == 0
- Case-insensitive: iso3166_alpha2_name('us') still works

---

## 3  `html`  M  (Document)

**Goal.** HTML  text extraction + CSS-selector queries. Real
need: indexing scraped HTML for FTS, extracting structured data
from page bodies, sanitizing user-authored HTML to plain text.

**Functions.**
```
html_to_text(html)                  -> text  (strip tags + decode entities)
html_get_text(html, selector)       -> text  (CSS selector, first match's text)
html_get_attr(html, selector, attr) -> text  (first match's attribute value)
html_all_text(html, selector)       -> text  (JSON array of all matches' text)
html_decode_entities(s)             -> text
html_encode_entities(s)             -> text
html_strip_tags(s)                  -> text  (tag-strip only, no entity decode)
html_links(html)                    -> text  (JSON array of href values)
html_images(html)                   -> text  (JSON array of {src, alt})
html_title(html)                    -> text  (first <title> contents)
html_version()                      -> text
```

**Crates.** `scraper` 0.20 (HTML5 parser built on html5ever; the
de-facto rust HTML parsing crate, what Servo uses). Plus
`html-escape` 0.2 for entity encode/decode.

**Scope.** ~half day.

**Acceptance.**
- html_to_text('<p>hi</p>') == 'hi'
- html_get_text('<p class="x">a</p><p>b</p>', '.x') == 'a'
- html_get_attr('<a href="/x">L</a>', 'a', 'href') == '/x'
- html_decode_entities('&lt;b&gt;hi&amp;') == '<b>hi&'
- html_encode_entities('<b>') == '&lt;b&gt;'
- html_links('<a href="/a">L</a><a href="/b">M</a>') ==
  '["/a","/b"]'
- html_title('<html><head><title>T</title></head></html>') == 'T'
- malformed HTML still extracts text (html5ever is liberal)

---

## 4  `exif`  M  (Media)

**Goal.** EXIF metadata from image blobs. JEITA EXIF spec.
`image-meta` covers dimensions + format only; this is the
metadata-tag surface (camera, GPS, datetime, etc).

**Functions.**
```
exif_field(blob, tag_name)    -> text  (raw value for any tag; NULL if absent)
exif_datetime(blob)           -> text  (ISO 8601 from DateTimeOriginal)
exif_camera(blob)             -> text  (Make + Model joined)
exif_make(blob)               -> text
exif_model(blob)              -> text
exif_gps_lat(blob)            -> real  (signed decimal degrees)
exif_gps_lng(blob)            -> real  (signed decimal degrees)
exif_orientation(blob)        -> integer (1-8 per EXIF spec)
exif_iso(blob)                -> integer
exif_aperture(blob)           -> real  (f-number)
exif_shutter_speed(blob)      -> text  (e.g. "1/250" or "0.5")
exif_focal_length(blob)       -> real  (mm)
exif_all(blob)                -> text  (JSON object of every tag)
exif_version()                -> text
```

NULL on blobs without EXIF (PNGs typically; some JPEGs). Each
fn parses the blob fresh; no shared state.

**Crates.** `kamadak-exif` 0.6 (pure-rust EXIF parser; the
established choice).

**Scope.** ~half day.

**Acceptance.**
- A JPEG fixture with known EXIF: camera, datetime, GPS
  all match
- A PNG blob: every fn returns NULL (no EXIF segment)
- Random bytes: every fn returns NULL (no error)
- exif_orientation in range 1..=8
- exif_gps_lat / lng return signed decimal degrees (South /
  West negative)

---

## 5  `color`  M  (Media)

**Goal.** Color-space conversions + CSS named colors + WCAG
contrast / luminance. Useful for design tools, palette work,
accessibility checks.

**Functions.**
```
color_parse(s)              -> text   (canonical '#RRGGBB' from any hex/name/rgb()/hsl() input)
color_rgb_to_hex(r,g,b)     -> text   ('#RRGGBB')
color_hex_to_rgb(hex)       -> text   ('[r,g,b]')
color_rgb_to_hsl(r,g,b)     -> text   ('[h,s,l]')
color_hsl_to_rgb(h,s,l)     -> text   ('[r,g,b]')
color_rgb_to_hsv(r,g,b)     -> text   ('[h,s,v]')
color_hsv_to_rgb(h,s,v)     -> text   ('[r,g,b]')
color_named(name)           -> text   (CSS named color  '#RRGGBB'; NULL if unknown)
color_luminance(color)      -> real   (relative luminance per WCAG 2.x, 0..1)
color_contrast_ratio(a,b)   -> real   (WCAG 2.x contrast ratio, 1..21)
color_mix(a, b, t)          -> text   (linear RGB interpolation, t in 0..1)
color_invert(color)         -> text   ('#RRGGBB' with each channel inverted)
color_version()             -> text
```

Color input accepted as `#RGB` / `#RRGGBB` / named ('red') /
`rgb(255,0,0)` / `hsl(0,100%,50%)`. NULL  NULL on each.

**Crates.** `csscolorparser` 0.7 (parser) + `palette` 0.7
(conversions) OR roll-own from the textbook formulas (color
math is ~200 lines).

**Scope.** ~half day.

**Acceptance.**
- color_parse('red') == '#ff0000'
- color_parse('#F00') == '#ff0000'
- color_rgb_to_hsl(255, 0, 0) == '[0, 100, 50]' (within
  rounding tolerance documented in smoke)
- color_named('rebeccapurple') == '#663399'
- color_luminance('#ffffff') == 1.0
- color_luminance('#000000') == 0.0
- color_contrast_ratio('#ffffff', '#000000') == 21
- color_invert('#000000') == '#ffffff'
- color_mix('#000000', '#ffffff', 0.5) == '#808080' (gamma-
  correct; document if the impl is linear or sRGB)

---

## 6  `qrcode`  S  (Media)

**Goal.** QR code generation. ISO/IEC 18004. Natural pair with
the `totp_url()` you ship in `totp`  generate a scannable QR
for `otpauth://` URIs in pure SQL.

**Functions.**
```
qr_svg(text, [ecc])         -> text    (full SVG document)
qr_unicode(text, [ecc])     -> text    (terminal-friendly, double-line blocks)
qr_modules(text, [ecc])     -> text    (JSON 2D 0/1 grid)
qr_size(text, [ecc])        -> integer (modules per side after encoding)
qr_version_for(text, [ecc]) -> integer (QR symbol version 1-40)
qrcode_version()            -> text
```

`ecc` (error correction level): `'L'` (~7%), `'M'` (~15%, default),
`'Q'` (~25%), `'H'` (~30%). NULL  NULL.

**Crates.** `qrcode` 0.14 (pure-rust; supports SVG + unicode +
bitmap output).

**Scope.** ~3 hours.

**Acceptance.**
- qr_svg('hello') returns text starting with `<svg `
- qr_size('hello') >= 21 (smallest QR is 21x21)
- qr_modules returns a JSON array of arrays of 0/1
- Larger input  larger symbol version (qr_version_for
  monotonic)
- Different ECC level for same text  may change version

---

## 7  `dist`  M  (Statistics)

**Goal.** Statistical distributions: PDF / CDF / inverse-CDF for
the common families. `stats` covers aggregates over data; this
covers the analytical distributions.

**Functions.**
```
normal_pdf(x, mean, std)       -> real
normal_cdf(x, mean, std)       -> real
normal_inv(p, mean, std)       -> real
poisson_pmf(k, lambda)         -> real
poisson_cdf(k, lambda)         -> real
binomial_pmf(k, n, p)          -> real
binomial_cdf(k, n, p)          -> real
exp_pdf(x, lambda)             -> real
exp_cdf(x, lambda)             -> real
chi_squared_pdf(x, k)          -> real
chi_squared_cdf(x, k)          -> real
beta_pdf(x, a, b)              -> real
beta_cdf(x, a, b)              -> real
gamma_pdf(x, shape, scale)     -> real
gamma_cdf(x, shape, scale)     -> real
t_pdf(x, df)                   -> real
t_cdf(x, df)                   -> real
dist_version()                 -> text
```

NULL on any out-of-domain input (negative variance, negative k,
etc) rather than NaN / error.

**Crates.** `statrs` 0.17 (pure-rust; the de-facto statistics
library).

**Scope.** ~half day.

**Acceptance.**
- normal_cdf(0, 0, 1) == 0.5 (symmetric standard normal)
- normal_inv(0.5, 0, 1) == 0
- normal_cdf(1.96, 0, 1) ~ 0.975 (two-tailed 95%)
- poisson_pmf(0, 1.0) == 1/e ~ 0.3679
- binomial_cdf(10, 10, 0.5) == 1.0
- exp_cdf(1.0, 1.0) == 1 - 1/e
- chi_squared_cdf with df=1 at x=3.841 ~ 0.95
- Tolerances documented in smoke.expected (4 decimal places)

---

## 8  `lang-detect`  S  (Text)

**Goal.** Language detection from text. n-gram heuristic, gives
ISO 639 codes. Pairs with `stemmer` for multi-language pipelines.

**Caveat (document explicitly):** Short strings ( ~20 chars)
are unreliable by nature; mixed-script text returns the dominant
script. Not a tokenizer; not a transliteration tool.

**Functions.**
```
lang_detect(text)             -> text   (ISO 639-3 code, 'eng', 'fra', etc; NULL if too short or unknown)
lang_detect_alpha2(text)      -> text   (ISO 639-1 code, 'en', 'fr', etc; NULL if no alpha-2)
lang_detect_confidence(text)  -> real   (0..1; how confident the detector is)
lang_detect_script(text)      -> text   (e.g. 'Latin' | 'Cyrillic' | 'Han' | 'Arabic')
lang_detect_all(text)         -> text   (JSON array, top-3 candidates with confidence)
lang_supported()              -> text   (JSON array of supported ISO 639-3 codes)
lang_detect_version()         -> text
```

**Crates.** `whatlang` 0.16 (pure-rust, ~70 languages, n-gram
based; fast, small dataset).

**Scope.** ~3 hours.

**Acceptance.**
- lang_detect('The quick brown fox jumps over the lazy dog') ==
  'eng' (or 'en' via alpha2 fn)
- lang_detect('') == NULL (too short / empty)
- lang_detect('a') == NULL (too short)
- lang_detect_script('') == NULL
- lang_detect_script('') == 'Cyrillic'
- lang_detect_script('') == 'Han'
- lang_detect a paragraph of French  'fra' / 'fr'
- lang_supported returns >= 50 codes

---

## Sequencing

Launch all 8 in parallel; ~510 wall-clock minutes. Same shape
as the prior two rounds.

## Risks

| Risk | Mitigation |
|---|---|
| iso-codes bundled data is ~50 KB (countries) + ~10 KB (currencies) | Acceptable for opt-in `.load` |
| html: scraper pulls html5ever which is ~500 KB of code | Acceptable for opt-in |
| exif: kamadak-exif handles HEIF partially | Document supported formats in src |
| color: gamma-correct vs sRGB mix can surprise | Document the mix model in src |
| qr: SVG output is portable; PNG would need image crate (deferred) | Acceptable; SVG covers >95% of use |
| dist: statrs uses approximations near domain boundaries | Document tolerance in smoke |
| lang-detect: short / mixed-script text unreliable | Documented in goals; smoke covers minimum input length |
| iban: BBAN per-country structure rules are 60+ entries | iban_validate crate has them all; we just expose |

## Acceptance for the plan itself

- 8 new `extensions/NAME/` crates exist on main
- `make ext-smoke-all` is green (69  77 total)
- Each commit references its plan item number
