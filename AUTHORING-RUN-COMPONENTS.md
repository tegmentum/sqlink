# Authoring Runnable Functions

A **runnable component** is a tiny wasm component that resolves shared
providers at runtime via `compose:dynlink/linker`. Functions are
typically **~150 KB**, vs. the ~12 KB-each-but-needs-the-2MB-cli
shape that the `sqlite:extension`-world extensions take. The
runtime (SQLite, common libs) is shared across all functions
loaded into the same `sqlink` session.

## When to write a runnable component vs. an extension

| | sqlite:extension-world ext | runnable component |
|---|---|---|
| Need to register SQL functions / aggregates / collations / hooks | yes | no |
| Need to run an ad-hoc procedure that uses SQL once | overkill | yes |
| Want to use compose's shared providers (std-text, std-hashing, …) | not yet | yes — register with `.register-provider` |
| Cross-language authoring (any language with cargo-component equivalent) | Rust-leaning today | any |
| Binary size | 12 KB shape + 2 MB bundled SQLite at runtime | ~150 KB; SQLite shared |

Use an extension when you're adding to the SQL language the user
types at the cli. Use a runnable component when you want a single
operation invoked via `.run <path>` (or, later, via dispatch).

## Crate layout

```
my-tool/
├── Cargo.toml
├── src/
│   └── lib.rs
└── wit/
    ├── world.wit
    └── deps/
        ├── compose-dynlink/      vendored from sqlite-wasm/wit/deps/
        ├── sys-compose/          vendored from sqlite-wasm/wit/deps/
        └── sqlite-wasm/          contains the run.wit defining the
                                   runnable world (interface run + run())
```

## Cargo.toml

```toml
[package]
name = "my-tool"
version = "0.1.0"
edition = "2021"

[dependencies]
wit-bindgen-rt = { version = "0.44.0", features = ["bitflags"] }
ciborium = { version = "0.2", default-features = false }

[lib]
crate-type = ["cdylib"]

[package.metadata.component]
package = "compose:my-tool"

[package.metadata.component.target]
path = "wit"
world = "impl"

[package.metadata.component.target.dependencies]
"compose:dynlink" = { path = "wit/deps/compose-dynlink" }
"sys:compose" = { path = "wit/deps/sys-compose" }
"sqlite:wasm" = { path = "wit/deps/sqlite-wasm" }

[profile.release]
opt-level = "s"
lto = true
strip = true
```

## world.wit

```wit
package compose:my-tool@0.1.0;

world impl {
    import compose:dynlink/linker@0.1.0;
    export sqlite:wasm/run@0.1.0;
}
```

## src/lib.rs

```rust
#[allow(warnings)]
mod bindings;

use ciborium::value::Value as CborValue;
use bindings::compose::dynlink::linker;
use bindings::exports::sqlite::wasm::run::Guest;

struct MyTool;

impl Guest for MyTool {
    fn run() -> Result<String, String> {
        // Step 1: resolve a shared provider by name.
        let sqlite = linker::resolve_by_id("sqlite-runtime")
            .map_err(|e| format!("resolve: {}", e.message))?;

        // Step 2: build a CBOR payload per the protocol in
        // sqlite-wasm/host/COMPOSE-PROTOCOL.md
        let req = CborValue::Map(vec![
            (CborValue::Text("sql".into()),
             CborValue::Text("SELECT 42".into())),
            (CborValue::Text("params".into()), CborValue::Array(vec![])),
        ]);
        let mut payload = Vec::new();
        ciborium::ser::into_writer(&req, &mut payload)
            .map_err(|e| format!("encode: {e}"))?;

        // Step 3: invoke the provider's endpoint.
        let resp_bytes = sqlite.invoke("query-scalar", &payload)
            .map_err(|e| format!("invoke: {}", e.message))?;

        // Step 4: decode + return.
        let resp: CborValue = ciborium::de::from_reader(&*resp_bytes)
            .map_err(|e| format!("decode: {e}"))?;
        let n = match resp {
            CborValue::Integer(i) => {
                let v: i128 = i.into();
                v as i64
            }
            _ => return Err("expected integer".into()),
        };

        Ok(format!("The answer is {n}"))
    }
}

bindings::export!(MyTool with_types_in bindings);
```

## Build + run

```sh
$ cargo component build --release
$ ls -lh target/wasm32-wasip1/release/my_tool.wasm
-rw-r--r--  151K  ...

$ sqlink --reactor --db /tmp/data.db sqlite-cli.wasm
sqlite> .run /path/to/my_tool.wasm
The answer is 42
```

## Available providers

| id | what | how it's wired | docs |
|---|---|---|---|
| `sqlite-runtime` | SQL execution against the cli's db | host shim (built-in) | `host/COMPOSE-PROTOCOL.md` |
| `std-text` | upper/lower/reverse/len on a UTF-8 string | wasm-component; register with `.register-provider std-text PATH` | source: `sqlite-wasm-loader/runtimes/wasmtime/std-text` |

`std-text` is the reference real-component provider. It targets
`compose:dynlink/dynlink-provider` (exports `endpoint`) and is
registered via `.register-provider <id> <path>` — the host compiles
the component once at registration time and instantiates it in a
fresh Store on every invoke. Same calling convention as
`sqlite-runtime` (CBOR payload, opaque-bytes return).

Add more providers by writing a wasm component that exports
`compose:dynlink/endpoint.handle(method, payload)` and registering
it under any id you choose. Existing example: see `fiji-text-demo`
(out-of-tree submodule artifact, will be renamed alongside the
submodule rebuild) for a runnable component that uses both
`sqlite-runtime` and `std-text` in one `run()`.

## Limits

- **Each invocation gets a fresh Store.** No state survives between
  `.run` calls. If you need persistent state, use the
  `sqlite-runtime` provider and write to the db.
- **One entry point.** A runnable component exports `run() -> result<string,
  string>`. If you need a function with parameters, take them via
  the `sqlite-runtime` provider's query interface (read a config
  table, e.g.), or use the `sqlite:extension`-world path instead.
- **No direct host capabilities.** A runnable component can't open
  files, make HTTP calls, etc. — its surface is whatever providers
  resolve. Want HTTP? Get a `std-http` provider. None today; that's
  a follow-up.
- **WASI is included** (cargo-component pulls it in), but the
  function shouldn't rely on stdio for output. Use the return
  string.

## Provider authoring

Out of scope for this guide (CP-following work in
PLAN-compose-integration.md). The pattern: target the
`compose:dynlink/dynlink-provider` world, export
`compose:dynlink/endpoint`, and follow the CBOR method shape
documented in `host/COMPOSE-PROTOCOL.md`.
