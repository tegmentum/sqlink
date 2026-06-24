# Excel extension wit-bindgen-4 mismatch — root-cause audit

## Symptom

`smoke_excel` (extension-smoke scenarios 1 + 2) fails with:

```
Error loading .../excel_extension.component.wasm: instantiate
loaded ext: failed to convert function to given type (code 1)
```

The other 207 extensions in the same matrix load cleanly.

## Root cause

Two facts compose into the failure:

1. **Excel's source builds only with `RUSTC_BOOTSTRAP=1` on stable
   rustc.** The transitive `typed-path` dep (via `zip 7.2.0`, via
   `calamine 0.35`) uses `std::os::wasi` directly, and
   `std::os::wasi` is itself nightly-gated on stable. Without
   `RUSTC_BOOTSTRAP=1` the build fails with `error[E0658]: use of
   unstable library feature 'wasip2'`. Documented in
   `tooling/compat-registry.json` under `typed-path` and `calamine`.

2. **The #439 / #440 substrate-rebuild commits added the `s3`
   variant to `sqlite:extension/policy@0.1.0`'s `capability`
   enum.** Every extension's manifest references this enum
   (`declared-capabilities: list<capability>`). The host
   instantiates with the new shape; an extension whose .wasm was
   built against the pre-#440 shape (13 variants) doesn't match
   the host's expected 14-variant signature → wasmtime's
   instantiate trips the "failed to convert function to given
   type" assertion.

Excel fell off the bulk-rebuild path that landed alongside #440
because the rebuild scripts (`make ext`, encode-extension-
components.sh) do not set `RUSTC_BOOTSTRAP=1` by default. The
build silently no-op'd, the existing pre-#440 `.wasm` stayed in
target/, and the encode step re-wrapped the stale bytes into a
`.component.wasm` with the old WIT shape. Every other extension
in the catalog built cleanly without bootstrap and picked up the
new policy variant.

Inspecting the on-disk artifact confirms the diagnosis:

```
$ wasm-tools component wit \
    extensions/excel/target/wasm32-wasip2/release/excel_extension.component.wasm \
    | grep -A 20 'variant capability'
    variant capability {
      spi, prepared, transaction, schema, state, cache, random,
      text, hashing, encoding, http, dns, wal-frames,
    }   # 13 variants — `s3` missing
```

vs. a freshly-built csv extension:

```
    variant capability {
      ... wal-frames, s3,   # 14 variants
    }
```

## Fix

Add `extensions/excel/.cargo/config.toml.template` that sets
`RUSTC_BOOTSTRAP = { value = "1", force = true }` under `[env]`.
The `setup-cargo-config.sh` script materializes the template
into a (gitignored) `config.toml` on first checkout; from that
point on, every `cargo build` in `extensions/excel/` — including
the bulk-rebuild loops — picks up the bootstrap env without the
caller having to remember the flag.

The fix is local to `extensions/excel/`. No source code, no
manifest, no WIT contract changes. The .wasm rebuild itself is
mechanical (`cargo build --target wasm32-wasip2 --release` +
`wasm-tools component new`); both happen inside the existing
extension build pipeline.

## Resolves

Closes the wit-bindgen-4 mismatch wedge tracked as #442. Brings
scenarios 1 + 2 smoke from 207 / 208 → 208 / 208.
