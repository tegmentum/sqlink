# Plan: user-selectable bake-in extensions

> **Status: contract + tooling shipped, sha3 reference port shipped.**
> Bake-in saves ~2.7 us per scalar call vs `.load`'d WIT equivalent.

## Motivation

Per-row scalar calls cross the wit-bindgen canonical ABI for any
extension loaded via `.load EXT.wasm`. PLAN-benchmarks.md measured
that crossing at ~2.7 us  about 25-30x the cost of a native
sqlite scalar dispatch (~100 ns). For tight scalar loops it
dominates: `SELECT sha3(name) FROM t` over 100k rows = ~270 ms
of pure boundary overhead.

The bake-in path lets a user pick extensions at build time and
compile them DIRECTLY into the cli's wasm component, registered
via `sqlite3_create_function_v2` at startup instead of via the
wasi component model at runtime.

## Bench: WIT vs baked

Same workload (`SELECT sum(length(sha3_256(name))) FROM t`),
100k rows, 5 trials, median, `.cwasm` (precompiled):

| Path | wasm | per-row marginal |
|---|---:|---:|
| `ext-scalar`  via `.load EXT.wasm` (WIT) | 951 ms | 9.5 us |
| `baked-scalar`  baked at compile time | 679 ms | 6.8 us |

Bake-in saves **2.7 us per call** at steady state. For a 100k-row
loop, that's 270 ms. Matches the WIT boundary estimate to within
noise.

## User-facing UX

```
$ tooling/compose-cli.py --list
sha3

$ tooling/compose-cli.py --bake sha3 --precompile
Baking: sha3
  cargo build --release -p sqlite-cli --target wasm32-wasip2 --features bake-sha3
  wasm-tools component new  sqlite_cli_baked.component.wasm
  precompile  sqlite_cli_baked.component.cwasm
wrote target/wasm32-wasip2/release/sqlite_cli_baked.component.wasm
wrote target/wasm32-wasip2/release/sqlite_cli_baked.component.cwasm

$ sqlite-wasm-run sqlite_cli_baked.component.cwasm --db x.db
sqlite> SELECT sha3_256('hello');
2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
```

No `.load` needed; `sha3_256()` is registered at cli startup.

## Bakeable-extension contract

An extension opts in by:

1. **Hoisting its algorithm to crate-level** (not just inside the
   `#[cfg(target_arch="wasm32")] mod wasm_export`). Both the WIT
   build and the bake build reuse it  guarantees they can't drift.

2. **Adding a `bake` cargo feature** in its `Cargo.toml`:
   ```
   [features]
   bake = ["dep:libsqlite3-sys"]

   [dependencies]
   libsqlite3-sys = { version = "0.38", optional = true, default-features = false }
   ```
   `default-features = false` is critical  the cli's tree already
   brings in bundled sqlite3 via `core`, and a second source would
   trip cargo's `links =` uniqueness rule.

3. **Implementing `src/bake.rs`** with:
   ```rust
   pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int
   ```
   The function calls `sqlite3_create_function_v2` for each scalar
   (or `_aggregate`/`_window` once those land) the extension exposes.
   Hot path: avoid per-call allocations  `hex::encode` over
   `format!()` cuts ~10x for hash-style scalars.

4. **Declaring no `[workspace]` line** in its Cargo.toml. If the
   extension had one to stay standalone, drop it; the cli's path
   dep needs the ext inside the parent workspace.

That's it. The wasi component build path keeps working unchanged
because the `bake` feature is off by default; `make ext NAME=ext`
produces the same `.component.wasm` as before.

## How the cli wires it

A single cargo feature per bakeable extension under
`[features]` in `cli/Cargo.toml`:
```
bake-sha3 = ["dep:sha3-extension"]
```

The cli's `ensure_cli_conn` calls a `register_baked_extensions(db)`
function which is just a stack of `#[cfg(feature = "bake-X")]`
blocks calling each extension's `register_into`. The body is
trivial; the gating decides what reaches the binary.

```rust
unsafe fn register_baked_extensions(_db: *mut libsqlite3_sys::sqlite3) {
    #[cfg(feature = "bake-sha3")]
    {
        let rc = sha3_extension::bake::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("bake-sha3: register_into failed rc={rc}");
        }
    }
}
```

## Trade-offs

| | WIT (`.load`) | Bake-in |
|---|---|---|
| Per-call overhead | ~2.7 us | ~100 ns |
| Hot-reload (`.unload` + re-`.load`) | yes | no |
| Binary size impact | none (separate .wasm) | grows per extension |
| Same .wasm for everyone | yes | no  user-specific |
| Build complexity | runtime `.load` | extra cargo feature per ext |
| Capability sandboxing | yes (`--grant`) | no  baked = trusted |

Bake-in is best for extensions a deployment ALWAYS wants. For
exploratory / occasional use, stay on `.load`  the WIT cost is
real but doesn't matter unless you're in a tight scalar loop.

## Currently bakeable

- `sha3`  reference implementation

## Adopting more

The mechanical work per extension is small (~30 min):
1. Hoist algorithm out of the `wasm_export` module.
2. Add `bake` feature + `libsqlite3-sys` optional dep to Cargo.toml.
3. Write `src/bake.rs` with `register_into`. The shape is
   stamped: `value_bytes`/`value_int` to extract args,
   `result_text`/`result_blob` for output, one `unsafe extern "C"`
   thunk per scalar. Aggregates need `_aggregate_function_v2` 
   not yet ported here.
4. Add a `bake-<name>` feature + optional dep entry to
   `cli/Cargo.toml`, mirror the registration call in
   `register_baked_extensions`.

Good candidates (popular, scalar-heavy): hyperloglog, count_min,
sketches, uuid, sha3, sha2, base64, crypto, regex.
