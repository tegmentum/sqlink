# Authoring Dot-Command Components

A **dot-command component** is a small wasm component that targets
the `dotcmd-aware` world and registers one or more entries in its
manifest's `dot-commands` list. The cli's three entry points
(interactive shell, argv subcommand, `dot_command()` SQL) all
dispatch through the same registry, so one declaration gets you
all three for free.

Reference extensions: `extensions/greet/` (hello-world, no SQL)
and `extensions/core-dotcmd/` (every built-in `.tables` /
`.schema` / `.echo` / `.timer` etc. lives here today). Both make
good copies-as-starter-templates.

## When to write a dot-command component vs. the other shapes

| | sqlite:extension scalar/vtab | runnable component | **dot-command component** |
|---|---|---|---|
| Adds a SQL surface (functions, vtabs, hooks) | yes | no | optional |
| Invoked by typing `.NAME args` at the cli | no | no | **yes** |
| Invoked via `sqlink .NAME args` on the command line | no | no | **yes (argv mode)** |
| Invoked via `SELECT dot_command('NAME', ...)` | no | no | **yes (SQL mode)** |
| Mutates cli session state (mode, headers, prompt) | no | no | **yes — emit state-deltas** |
| Reads cli session state (current mode, output file) | no | no | **yes — `cli-state.get-*`** |
| Sandboxed | yes | yes | yes |

Use a dot-command component when:
- you want to type something at the `sqlite>` prompt and have a
  wasm component handle it,
- the command is a one-shot operation (write a row, dump a table,
  validate schema) rather than a SQL function,
- you want the same invocation to work in scripts (argv) and in
  SQL contexts without writing it three times.

The PLAN-dotcmd-plugins.md "Layer 2" (database-resident registry)
means a dot-command extension installed via `.sqlink install
file:///path/to/foo.wasm` is *persisted in the user's db*: a fresh
cli session against the same db transparently resolves the
command on first use. Bundle the artifact with `--bundle` (the
default) for a portable db; unbundle to keep just the metadata
and resolve via `.sqlink resolver` later.

## Crate layout

```
my-dotcmd/
├── Cargo.toml
├── src/
│   └── lib.rs
└── (no local wit/ — the path resolves to the shared
    sqlite-loader-wit submodule that defines `dotcmd-aware`)
```

Unlike runnable components, dot-command components use plain
`cargo build --target wasm32-wasip2` (no `cargo component build`
wrapper). `wit-bindgen::generate!` produces the bindings inline.

## Cargo.toml

```toml
[package]
name = "my-dotcmd-extension"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.49"

[workspace]   # standalone; not in the sqlite-wasm workspace
```

Note: every extension targeting one of the `sqlite-loader-wit`
worlds is a standalone workspace so the parent repo's exclude
list keeps it from being pulled into the cli build by mistake.
Build it from its own directory:

```sh
cd extensions/my-dotcmd && cargo build --target wasm32-wasip2 --release
wasm-tools component new \
    target/wasm32-wasip2/release/my_dotcmd_extension.wasm \
    -o target/wasm32-wasip2/release/my_dotcmd_extension.component.wasm
```

## src/lib.rs

The skeleton is three guest implementations: `MetadataGuest`
(declare the manifest), `ScalarFunctionGuest` (stub if no SQL
surface), `DotCommandGuest` (handle `invoke`).

```rust
extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "dotcmd-aware",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::dot_command::{
        Guest as DotCommandGuest, InvokeContext, InvokeResult, StateDelta,
    };
    use bindings::exports::sqlite::extension::metadata::{
        DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::cli_stdout;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    // Stable identifiers for each registered command. Keep them
    // unique within the extension; the cli routes invoke(func_id, …)
    // back to the matching arm.
    const FID_HELLO: u64 = 1;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "my-dotcmd".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![DotCommandSpec {
                    id: FID_HELLO,
                    name: "hello".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    summary: "Print a greeting".into(),
                    usage: "hello [NAME]".into(),
                    help: "Writes 'hello, NAME!' to stdout. NAME defaults to 'world'.".into(),
                    examples: alloc::vec![],
                    requires_write: false,
                    no_args: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("my-dotcmd: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            match func_id {
                FID_HELLO => {
                    let name = if ctx.args.trim().is_empty() { "world" } else { ctx.args.trim() };
                    cli_stdout::write(&format!("hello, {name}!\n"));
                    Ok(InvokeResult {
                        text: String::new(),       // streamed via cli_stdout above
                        state_deltas: alloc::vec![],
                        ok: true,
                        exit_code: 0,
                    })
                }
                _ => Err(SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("my-dotcmd: unknown func id {func_id}"),
                }),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
```

## Three ways to write output

1. **Streamed via `cli-stdout::write`.** Lines land on the host's
   stdout as the invoke runs; useful for large results or
   progressive output.
2. **Trailing text via `InvokeResult.text`.** Returned to the
   dispatcher after invoke finishes; the cli appends it to the
   output channel. Use this when the output is small and you
   want it to flow through `.output FILE` or `.once FILE`.
3. **`cli-stderr::write`.** Goes to the host's stderr regardless
   of `.output` redirection. Use for diagnostics, not data.

Pick streamed for "many rows", trailing for "small message",
stderr for "this isn't part of the result".

## SQL access via `spi.execute`

The dotcmd-aware world imports `sqlite:extension/spi`, which the
host wires to the cli's user db. `spi::execute(sql, params)`
returns rows the same shape as the prepare/step API.

```rust
use bindings::sqlite::extension::spi;
use bindings::sqlite::extension::types::SqlValue;

let result = spi::execute(
    "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE ?1",
    &[SqlValue::Text(pattern.into())],
)?;
for row in &result.rows {
    if let Some(SqlValue::Text(t)) = row.first() {
        cli_stdout::write(t);
        cli_stdout::write("\n");
    }
}
```

`spi.execute` errors are `SqliteError` (`code`, `extended_code`,
`message`); propagate them by returning `Err(_)` from `invoke` or
catch and produce a user-friendly `text` line. See
`extensions/core-dotcmd/src/lib.rs` for the recurring pattern.

## Mutating cli session state

The dotcmd-aware world exports `cli-state`, but writes don't go
through that import — they're declarative side-effects on the
invoke result. The cli's dispatcher reads
`InvokeResult.state_deltas`, decodes each delta's value, and
applies it to its `SETTINGS` thread-local.

Pattern:

```rust
fn cmd_mode_csv() -> InvokeResult {
    InvokeResult {
        text: String::new(),
        state_deltas: alloc::vec![StateDelta {
            key: "display/mode".into(),
            value: SqlValue::Text("csv".into()),
        }],
        ok: true,
        exit_code: 0,
    }
}
```

State keys are slash-namespaced — see the schema below. Unknown
keys are silently ignored by the cli (forward-compatible: an
extension that emits a future-typed delta runs without breaking).

### State schema

Today's cli applies the following deltas. Anything else is a
no-op at the cli (the host may surface it on
`cli-state.get` for read-side lookups in a future release).

| key                  | type | meaning |
|----------------------|------|---------|
| `io/echo`            | bool | echo each input line before execution |
| `io/headers`         | bool | column headers in list/csv/tabs |
| `io/timer`           | bool | `Run Time: real X.XXX` after each statement |
| `io/stats`           | bool | `Memory Used: N bytes` after each statement |
| `io/changes`         | bool | `changes: N total_changes: M` after each statement |
| `io/binary`          | bool | render BLOBs as `X'…'` hex literals |
| `io/eqp`             | bool | prepend `EXPLAIN QUERY PLAN` output |
| `io/explain`         | text | `on` / `off` / `auto` |
| `io/trace`           | bool | log expanded SQL per statement |
| `bail/on-error`      | bool | abort the rest of the script on first error |
| `display/mode`       | text | `list` / `csv` / `line` / `column` / `table` / `markdown` / `tabs` / `json` |
| `display/nullvalue`  | text | text rendered for SQL NULL |
| `display/separator`  | text | column separator in list/csv/tabs |
| `prompt/main`        | text | main prompt (`sqlite> ` by default) |
| `prompt/cont`        | text | continuation prompt (`   ...> ` by default) |

Boolean values go over the WIT boundary as `SqlValue::Integer(0)`
or `Integer(1)`. Text values go as `SqlValue::Text(...)`. The cli
side decodes the JSON encoding the host produces — extensions
don't need to think about the wire format.

## Manifest fields that matter

```rust
DotCommandSpec {
    id,             // u64; opaque to the cli, unique within the extension
    name,           // without leading dot; the dispatcher matches on this
    version,        // typically env!("CARGO_PKG_VERSION")
    summary,        // one-line; surfaced by `.help` and `.sqlink list`
    usage,          // synopsis, e.g. "tables [PATTERN]"
    help,           // multi-line; surfaced by `.help NAME`
    examples,       // Vec<DotCommandExample { description, command }>
    requires_write, // true if the cmd needs a writable db (cli enforces)
    no_args,        // true if any args at all are an error
}
```

`requires_write: true` is a hint today — the cli's enforcement
landing is on the Phase 5 follow-up list. `no_args` is enforced
before invoke is even called: passing args raises a usage error.

## Install + use

Once the component is built and wrapped with `wasm-tools
component new`, install it from any cli session:

```
sqlite> .sqlink install file:///path/to/my_dotcmd_extension.component.wasm
Installed my-dotcmd from file://… (12345 bytes, digest blake3:…):
  .hello

sqlite> .hello
hello, world!

sqlite> .hello Alice
hello, Alice!
```

The install goes into `sqlink_dotcmd` + `sqlink_artifact` in the
user's db. A fresh cli process against the same db will auto-
resolve `.hello` from the registry — no `.sqlink install` needed
the second time.

The same component is callable via argv:

```sh
$ sqlink --db user.db sqlite-cli.wasm .hello Alice
hello, Alice!
```

And via SQL:

```sql
SELECT dot_command('hello', 'Alice');
-- "hello, Alice!\n"
```

`dot_command()` returns the trailing `text` field only and
ignores state-deltas — state-mutating commands invoked from SQL
are a no-op on settings (the SQL call site doesn't have a session
to mutate).

## Smoke pattern

```sh
# Build + wrap the component.
cd extensions/my-dotcmd
cargo build --target wasm32-wasip2 --release
wasm-tools component new \
    target/wasm32-wasip2/release/my_dotcmd_extension.wasm \
    -o target/wasm32-wasip2/release/my_dotcmd_extension.component.wasm

# Smoke through the cli.
rm -f /tmp/smoke.db
cat <<EOF | sqlink --db /tmp/smoke.db sqlite_cli.component.wasm
.sqlink install file://$(pwd)/target/wasm32-wasip2/release/my_dotcmd_extension.component.wasm
.hello
.hello Alice
.exit
EOF
```

## Limits

- **One Store per dispatch.** The host caches the dotcmd-aware
  instance per extension; calls are sequential per extension and
  share the cache between calls. State that lives only in wasm
  memory persists across calls until the extension is unloaded.
- **No direct host capabilities.** A dot-command extension can't
  open files, make HTTP calls, or call back into the
  extension-loader (so `.sqlink` ships as a cli built-in for
  now). The `cli-stdout` / `cli-stderr` / `cli-state` / `spi`
  imports are the surface.
- **No leading dot in `DotCommandSpec.name`.** Use `"tables"`,
  not `".tables"`. The dispatcher adds the dot.
- **func_id collisions are silent.** If two `DotCommandSpec`s
  share an `id`, the cli routes to whichever the registry walk
  finds first. Use distinct ids.
- **Capabilities are declarative.** Sandboxed access (HTTP, DNS,
  fs, kv) goes through the unified policy + grants stored on
  `Manifest.declared_capabilities`. Today's dotcmd-aware world
  doesn't import those surfaces; widening is on the Phase 5
  follow-up list.

## See also

- `extensions/greet/` — minimal example
- `extensions/core-dotcmd/` — the canonical port of the cli's
  built-ins (~22 commands at last count)
- `PLAN-dotcmd-plugins.md` — full design rationale + the three-
  entry-point + Layer-2-registry decision history
- `AUTHORING-RUN-COMPONENTS.md` — sibling guide for the runnable
  one-shot shape
