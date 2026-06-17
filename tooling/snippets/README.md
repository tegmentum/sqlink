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
