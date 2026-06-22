# Plan: user-selectable embed extensions

> **Status: contract + tooling shipped, sha3 reference port shipped.**
> Embed saves ~2.7 us per scalar call vs `.load`'d WIT equivalent.

## Motivation

Per-row scalar calls cross the wit-bindgen canonical ABI for any
extension loaded via `.load EXT.wasm`. PLAN-benchmarks.md measured
that crossing at ~2.7 us  about 25-30x the cost of a native
sqlite scalar dispatch (~100 ns). For tight scalar loops it
dominates: `SELECT sha3(name) FROM t` over 100k rows = ~270 ms
of pure boundary overhead.

The embed path lets a user pick extensions at build time and
compile them DIRECTLY into the cli's wasm component, registered
via `sqlite3_create_function_v2` at startup instead of via the
wasi component model at runtime.

## Bench: WIT vs embedded

Same workload, 100k rows, 5 trials, median, `.cwasm`
(precompiled). Two extensions, same pattern:

| Workload | WIT `.load` | Embedded | Saved |
|---|---:|---:|---:|
| `SELECT sum(length(sha3_256(name))) FROM t` | 951 ms | 679 ms | 272 ms (29%) |
| `SELECT sum(length(uuidv4())) FROM t` | 833 ms | 612 ms | 221 ms (26%) |

Embed saves **~2.5-2.7 us per call** at steady state. For a
100k-row loop, that's a few hundred ms recovered. The numbers
hold across two unrelated extensions (hash function vs UUID
generator), so the win is on the dispatch path, not the work
itself.

## User-facing UX

Compose is a subcommand of the main `sqlite-wasm-run` binary 
SQLite's single-executable spirit. No separate Python tool to
install.

```
$ sqlite-wasm-run compose --list
sha3
uuid

$ sqlite-wasm-run compose --embed sha3,uuid --precompile
Baking: sha3, uuid
  cargo build --release -p sqlite-cli --target wasm32-wasip2 --features embed-sha3,embed-uuid
  wasm-tools component new  sqlite_cli_embedded.component.wasm
wrote target/wasm32-wasip2/release/sqlite_cli_embedded.component.wasm
  precompile  sqlite_cli_embedded.component.cwasm
wrote target/wasm32-wasip2/release/sqlite_cli_embedded.component.cwasm

$ sqlite-wasm-run sqlite_cli_embedded.component.cwasm --db x.db
sqlite> SELECT sha3_256(uuidv4());
efe1a2e33d99e1c6d4d8f31946243569b0d17cc9a1379e054dbd29660596c7b1
```

No `.load` needed; the embedded scalars are registered at cli
startup. **`.load` still works alongside** for anything not
embedded  the embed path doesn't disable the WIT loader:

```
sqlite> .load extensions/eval/target/wasm32-wasip2/release/eval_extension.component.wasm
Loaded extension: eval 0.1.0 ... (2 registered: 2 scalar)
sqlite> SELECT eval('SELECT 7');
7
sqlite> SELECT length(eval('SELECT ''' || sha3_256('x') || '''')) = 64;
1
```

Embedded + dynamic-load composition is regression-guarded by
`tooling/cli-smokes/embedded_plus_load.{sql,expected}`.

Default cli (no embed) keeps working as before; `.load` is the
only path to scalars there.

Run from the repo root, or pass `--repo-root PATH`. The
subcommand shells out to `cargo` + `wasm-tools` (both available
to anyone who built the cli in the first place).

## Embeddable-extension contract

An extension opts in by:

1. **Hoisting its algorithm to crate-level** (not just inside the
   `#[cfg(target_arch="wasm32")] mod wasm_export`). Both the WIT
   build and the embed build reuse it  guarantees they can't drift.

2. **Adding a `embed` cargo feature** in its `Cargo.toml`:
   ```
   [features]
   embed = ["dep:libsqlite3-sys"]

   [dependencies]
   libsqlite3-sys = { version = "0.38", optional = true, default-features = false }
   ```
   `default-features = false` is critical  the cli's tree already
   brings in bundled sqlite3 via `core`, and a second source would
   trip cargo's `links =` uniqueness rule.

3. **Implementing `src/embed.rs`** with:
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
because the `embed` feature is off by default; `make ext NAME=ext`
produces the same `.component.wasm` as before.

## How the cli wires it

A single cargo feature per embeddable extension under
`[features]` in `cli/Cargo.toml`:
```
embed-sha3 = ["dep:sha3-extension"]
```

The cli's `ensure_cli_conn` calls a `register_embedded_extensions(db)`
function which is just a stack of `#[cfg(feature = "embed-X")]`
blocks calling each extension's `register_into`. The body is
trivial; the gating decides what reaches the binary.

```rust
unsafe fn register_embedded_extensions(_db: *mut libsqlite3_sys::sqlite3) {
    #[cfg(feature = "embed-sha3")]
    {
        let rc = sha3_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-sha3: register_into failed rc={rc}");
        }
    }
}
```

## Trade-offs

| | WIT (`.load`) | Embed |
|---|---|---|
| Per-call overhead | ~2.7 us | ~100 ns |
| Hot-reload (`.unload` + re-`.load`) | yes | no |
| Binary size impact | none (separate .wasm) | grows per extension |
| Same .wasm for everyone | yes | no  user-specific |
| Build complexity | runtime `.load` | extra cargo feature per ext |
| Capability sandboxing | yes (`--grant`) | no  embedded = trusted |

Embed is best for extensions a deployment ALWAYS wants. For
exploratory / occasional use, stay on `.load`  the WIT cost is
real but doesn't matter unless you're in a tight scalar loop.

## Currently embeddable

- `sha3`  reference implementation
- `uuid`  second port; confirmed the pattern generalizes

## Adopting more

The mechanical work per extension is small (~30 min). uuid was
~20  the embed.rs is mostly typing.

1. Hoist algorithm out of the `wasm_export` module if it's there
   (sha3 needed this; uuid didn't because its algorithm IS the
   `uuid` crate).
2. Add `embed` feature + `libsqlite3-sys` optional dep to Cargo.toml.
3. **Gate `mod wasm_export` with `#[cfg(all(target_arch="wasm32", not(feature="embed")))]`.**
   Critical: two embedded extensions both compile `bindings::export!`
   which declares the same `sqlite:extension/metadata#describe`
   symbol  the linker rejects the duplicate. The embed path
   doesn't need the WIT exports anyway.
4. Write `src/embed.rs` with `register_into`. The shape is stamped:
   `value_text_bytes`/`value_int` to extract args,
   `result_text`/`result_blob` for output, one `unsafe extern "C"`
   thunk per scalar. Avoid per-call `format!()`  use `hex::encode`
   for hex output (10x faster on the hot path).
   Aggregates need `_aggregate_function_v2`  not yet ported here.
5. Drop any `[workspace]` declaration so the cli's path dep works.
6. Add a `embed-<name>` feature + optional dep entry to
   `cli/Cargo.toml`, mirror the registration call in
   `register_embedded_extensions`.

`sqlite-wasm-run compose --list` auto-discovers any extension
with a `embed` feature in its Cargo.toml + a `src/embed.rs`  no
manifest to maintain.

Good candidates (popular, scalar-heavy): hyperloglog, count_min,
sketches, sha2, base64, crypto, regex.

## Catalog rollout status (as of June 2026)

| Category | Count | Status |
|---|---:|---|
| Embedded + cli-wired + working | **68** | Compose with `--embed name1,name2,...` |
| embed.rs ships, blocked from cli wiring | 3 | `template` / `graphql` (rust-lld SIGSEGV); `ids` (needs wasi_snapshot_preview1 adapter for ulid/nanoid's std::time) |
| Scalar-only, not yet ported | 12 | Larger / dep-graph-heavy: avro, web-parsers, extfns, time, crypto-auth, parsers, geo, vec, crypto-keys, formats, onnx, text-nlp |
| Aggregate (needs contract extension) | 6 | hyperloglog, count_min, sketches, decimal, stats, postgis-bridge. Need `register_aggregates` helper in `sqlite-embed` using `sqlite3_create_aggregate_function_v2`. |
| Vtab (needs contract extension) | 18 | arrow, closure, completion, csv, define, excel, listargs, parquet, pmtiles, postgis-bridge, series, spellfix1, text-utils, time-series, trie, vec_each, vec0, zipfile. Need `register_vtabs` helper using `sqlite3_create_module_v2`. |
| Collation (needs contract extension) | 1 | uint. Need `register_collations` helper using `sqlite3_create_collation_v2`. |

### The 60 embedded extensions

```
aba           color         eval          isin          regexp
baseN         color         faker         iso           roman
bencode       cron          fileio        json1         semver
bic           crypto        geo-distance  latlon        sentiment
bpe           csscolor      hexdump       lorem         sha3
case          cusip         http          mac           sqlparse
codecs        currency      iban          mailto        ssn
color         dns           ical          morse         template
container     ean           idna          natsort       totype
creditcard    email         ieee754       numfmt        unitconv
crc           emoji         iban          phone         url
              escape        ipaddr        postcode      uuid
                                          radix         vin
                                                        zorder
```

### How the bulk port ran (June 2026)

- Reference ports written by hand: sha3 (with hoisting), uuid, crc.
- Centralized helpers built: `sqlite-embed` crate with `SqlValueOwned` +
  `ScalarSpec` + `register_scalars`, dispatching through ONE generic
  `sqlite3_user_data`-threaded thunk. Per-extension boilerplate
  dropped from ~150 lines to ~30-90.
- Parallel agents ported in batches: 6/12/12/10/10/8 per batch.
  Around 95% of ports landed clean. Cli wiring is the only
  step that needs serialization across a batch  trivial central merge.
- Two ports hit rust-lld SIGSEGV when their full graph entered the
  cli link (template, graphql). embed.rs files shipped so the moment
  lld grows up they're a one-line drop-back.
- Agent session limit ended batch 9 mid-run; finished the partial
  6 by sed-scripting the Cargo.toml + lib.rs gates (the embed.rs
  files the agents had already written were complete on disk).

### Extending the contract for non-scalar surfaces

Pattern is identical, just a different `sqlite3_create_*` API:

  * **Aggregates**: `sqlite3_create_aggregate_function_v2(db, name, narg,
    flags, ctx, xStep, xFinal, xDestroy)`. The shared crate gains
    `AggregateSpec { fid, name, narg, det }` and `register_aggregates`.
    Per-extension: a `step_scalar`/`final_scalar` pair that takes
    `&mut state` (per-aggregation context) and `args`.
  * **Collations**: `sqlite3_create_collation_v2(db, name, eTextRep, ctx,
    xCompare, xDestroy)`. The shared crate gains
    `CollationSpec { name }` and `register_collations`. Per-extension:
    a `compare(a: &str, b: &str) -> Ordering` fn.
  * **Vtabs**: `sqlite3_create_module_v2(db, name, &module, ctx,
    xDestroy)`. Most complex  the module has 19+ callbacks. Likely
    needs its own crate (`sqlite-embed-vtab`) rather than bolted on
    here.

Each surface roughly doubles the helper crate size but the per-
extension ports stay the same shape (algorithm + spec table +
one register call).
