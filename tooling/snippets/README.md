# Snippets

Small Rust helpers that get reused across extensions. Each
snippet is self-contained Rust source you can paste directly
into an extension's `lib.rs` (typically inside the
`mod wasm_export` block, alongside `arg_text`).

## Why snippets (not a crate)

Each extension is its own wasm component with its own pinned
wit-bindgen output. A shared `extensions-common` crate would
require workspace integration and complicate the per-extension
Cargo.toml. Snippets sidestep that  paste-once-and-own model.

Trade-off: a bugfix in a snippet doesn't auto-propagate. Track
which extensions use which snippet and patch in lockstep when
fixing. See "Consumers" below.

## When to add a snippet

- When you've written the same ~10-50 LOC helper in 2+
  extensions and the implementations should agree
- When the implementation has a tricky edge case future-you
  would benefit from a single annotated home for

When NOT to add:

- One-off helpers used in a single extension
- Anything that wants to be a real Rust crate (vendor it as
  one of the extension's `[dependencies]`)
- Anything with state across calls (snippets are stateless)

## Available snippets

### luhn.rs

Classic Luhn mod-10 check + variations. Three patterns:

- `luhn_validate(s: &str) -> bool`  classic credit-card-shape
  Luhn (alternate-from-second-from-right, double-and-sum-digits)
- `luhn_check_digit(s: &str) -> Option<u32>`  ISIN-shape Luhn
  (alternate STARTS from rightmost; computes the check digit
  that completes the sum mod 10 = 0)
- `weighted_mod10(s: &str, weights: &[u32]) -> Option<bool>` 
  generic weighted-sum mod-10 check (ABA, EAN, etc.)

Consumers: `parsers`, `creditcard`, `isin`, `aba`.

## Design patterns (not snippets)

These are shapes you'll keep wanting to write but they're too
small (or too context-dependent) to extract as inlinable code.
Documented here so future-you reaches for the right one
instead of inventing a worse version.

### Ordered classifier  "try each candidate in priority order"

When you have an input and want to identify which of N kinds
it matches (postcode  country, IP  region, MAC OUI 
vendor, brand from BIN range, etc.), the canonical shape is:

```rust
fn classify(input: &str) -> Option<&'static str> {
    let n = normalize(input);
    // Order matters: place specific patterns BEFORE patterns
    // they're subsets of, so the right key wins on overlap.
    for key in &["specific-1", "specific-2", "general"] {
        if predicate(key, &n) {
            return Some(key);
        }
    }
    None
}
```

Why not extract as a fn or macro:

- Generic helper would need `Fn(&str) -> bool` predicates;
  closures capturing context (regex tables, lookup maps)
  don't coerce to `fn` pointers, and boxed closures add
  per-call allocation
- A macro could preserve captures (`first_match! { ... }`) but
  hides an early-return which is a footgun
- The hand-written form is 5-7 lines, trivially debuggable

Consumers as of this writing: `postcode` (country detection),
`creditcard` (brand-by-BIN), `phone-prefix` (region by
international prefix). If a 3rd consumer with overlapping
input shape appears, revisit extraction  the `fn pointer
+ static table` form may finally win.

### Validator + extractor pair

Every check-digit-bearing identifier (ISIN, CUSIP, VIN, ISBN,
EAN, etc.) ends up with this shape:

```rust
pub fn validate(raw: &str) -> bool {
    let n = normalize(raw);
    if n.len() != EXPECTED_LEN { return false; }
    let (body, last) = n.split_at(EXPECTED_LEN - 1);
    let last_d = last.chars().next().and_then(|c| c.to_digit(10));
    match (check_digit(body), last_d) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => false,
    }
}
```

Followed by extractor scalars (`<id>_issuer`, `<id>_serial`,
etc.) that all gate on `if n.len() == EXPECTED_LEN { ... }
else { NULL }`.

Why not extract: the LENGTH and the slicing boundaries are
domain-specific; the check digit algorithm differs (Luhn vs
mod-11 vs custom). Sharing the SHELL would obscure those
real differences. Better to keep the pattern explicit so
each extension's `validate()` reads like a spec.
