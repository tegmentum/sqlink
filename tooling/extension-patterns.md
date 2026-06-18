# Extension patterns

When starting a new SQLite-wasm extension, the FIRST question is
"what shape is this?"  the answer determines the entire skeleton.
This file catalogs the shapes I've observed across the catalog so
new ships start from a known-good template.

If a new extension doesn't cleanly fit one of these, that's a
signal it's either (a) genuinely novel and worth documenting, or
(b) two shapes glued together and worth splitting.

## Quick picker

| Shape                    | When to use                                          | Reference |
|--------------------------|------------------------------------------------------|-----------|
| Classifier               | input  one of N kinds; order-of-tries matters    | postcode, phone-prefix, creditcard |
| Validator + extractor    | check-digit ID  validate + decompose into parts | vin, isin, cusip, bic, ean        |
| Parser-union             | many input forms  one canonical                  | color, latlon                     |
| Alias-table              | canonical concept w/ many synonyms  scale factor | unitconv                          |
| Exact-key lookup         | canonical 3/4-char code  facts                   | currency                          |
| Formatter + parser pair  | bidirectional human  machine                    | humansize                         |
| Pure formatter           | number/value  pretty string (no parse-back)      | numfmt                            |
| Coord transform          | numeric value in domain A  domain B (no lookup)   | latlon, geo-distance               |
| Base-N algorithm         | radix arithmetic / encoding                          | radix                              |
| Tokenize-then-compare    | string ordering by structure, not bytes              | natsort                            |

## Detailed shapes

### Classifier

`fn classify(input: &str) -> Option<Key>` walking a sorted table
in PRIORITY ORDER (most specific first). Each table row tests a
predicate. First match wins.

Why this shape: overlapping patterns where more-specific should
beat less-specific (e.g. `+1242 = BS` beats `+1 = US`).

Pitfalls:
- Order is load-bearing. Test that a specific pattern wins over
  its general superset.
- Predicate-by-closure doesn't compose into `fn` pointers; keep
  the loop hand-written. See tooling/snippets/README.md
  "Design patterns" for the rationale.

### Validator + extractor

Check-digit-bearing identifier:
```rust
pub fn validate(raw: &str) -> bool { ... }
pub fn issuer(raw: &str) -> Option<&str> {
    if !valid_length(&n) { return None; }
    Some(&n[..K])
}
```

Each extractor scalar gates on `validate(raw)` (or just on the
length / shape preconditions) before slicing. Wraps in NULL on
failure.

Reference algorithm comes from the spec (ISO 6166, ISO 9362,
ISO 3779). The check-digit math differs per spec  Luhn,
mod-11, mod-97  no shared crate.

### Parser-union

`fn parse(s: &str) -> Option<Domain>` tries several input forms
in succession: `#rgb`, then `rgb(...)`, then named color. Each
parser returns Option; first Some wins.

Different from classifier: no priority ordering needed (the
patterns are syntactically disjoint), just compose with `?` or
`.or_else()`.

### Alias-table (with optional affine path)

`(name, factor)` table where `name` is one of many synonyms for
a canonical unit. Conversion: `v * factor(from) / factor(to)`.
For affine quantities (Celsius / Fahrenheit / Kelvin) use a
separate `to_canonical()` + `from_canonical()` pair so offsets
don't collapse into the scale step.

The unit aliases ARE the canonicalization  no separate
`normalize()` step. Case-insensitive lookup.

### Exact-key lookup

Canonical 3- or 4-character code (ISO 4217 currency, ISO 3166
country) where the code itself is the lookup key. Pre-filter
inputs by length + character class so the lookup loop exits
fast on garbage.

Different from alias-table: the code IS the canonical name, no
synonyms.

### Formatter + parser pair

`format(n)  "1.5 KB"`; `parse("1.5 KB")  n`. The
"round-trip property" (`parse(format(n)) == n` for round
values) is a useful informal invariant  smoke it explicitly.

Use cases: humanized sizes (humansize), durations.

### Pure formatter

Just `fn(value)  String`, no parse-back. Trim trailing zeros
for display; handle negative + sign placement uniformly.

Use cases: ordinals ("21st"), commas, scientific notation,
percentages.

### Coord transform

Pure numeric mapping between coordinate systems. The mapping
function may need an extra "axis" or "system" arg (`'lat'` /
`'lon'`) to disambiguate which interval applies.

Watch the asymmetries: longitudes WRAP (180 - -180), latitudes
CLAMP (no wrap). Document.

### Base-N algorithm

`fn to_base(n, base) -> String` over a `&[u8; 36]` digit
alphabet. Handle the i64::MIN  i128 abs trick for sign
preservation. Out-of-range base  None.

### Tokenize-then-compare

Tokenizer splits input into a sequence of (Num | Text) parts;
comparator walks parallel sequences. The key function packs
each token into a bytewise-comparable form so `ORDER BY key(col)`
agrees with the comparator.

Use cases: natural sort, version compare, structured-text
ordering.

## Reusable helpers

These are the pieces shared across shapes  not snippets but
patterns common enough that you should recognize them:

- **`arg_text` / `arg_int` / `arg_real` / `arg_blob`** copy-paste
  into every extension. Returns `Result<T, String>` with a
  per-function error message. Scaffold ships them.
- **`#[allow(dead_code)]`** on arg helpers you don't use yet 
  the scaffold ships all four; delete what you don't need at
  the end.
- **`Option::map(SqlValue::Text).unwrap_or(SqlValue::Null)`** 
  the standard "fail-clean" output pattern. Pairs with T-19's
  `<NULL>` sentinel in smokes.

## Not-shape anti-patterns

These are choices that LOOK like a new shape but are actually
mistakes:

- **Adding a `_normalize` helper that you call from every
  scalar.** That's the alias-table or coord-transform shape
  wanting to emerge. Push the normalization into the lookup
  itself.
- **Returning a JSON-encoded multi-value where N fixed-purpose
  scalars would do.** SQLite is happier indexing on scalar
  columns. If callers always want `name AND symbol AND
  decimals`, ship three scalars, not one `currency_info(code)`
  that returns JSON.
- **Wrapping a heavy library to expose 1-2 functions.** If
  only one function ends up SQL-callable, paste the algorithm
  inline. The wasm component overhead vanishes; the dependency
  tax does not.

## Adding to this catalog

When a new ship doesn't fit any shape, write a new entry HERE
before committing. The ship's lessons-learned entry should
reference it. New shapes happen ~once per 5-10 ships in the
current cadence  not zero, not every ship.
