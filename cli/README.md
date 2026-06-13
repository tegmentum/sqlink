# sqlite-cli-rust

Reactor-shape Rust port of the SQLite CLI. Targets the
`sqlite-cli-reactor` world; SQLite is bundled via rusqlite.

## Why a reactor, not command-mode

The host (`sqlite-wasm-run`) drives the REPL. The CLI exports
`init`/`eval`/`is-done`/etc., the host calls them per line of user
input. This control-flow inversion is what makes the in-WASM
`spi.execute` re-entry pattern possible (see
`PLAN-reactor-cli-async-host.md` and `host/SPI.md` for the
architectural detail).

## Why `wasmtime run` doesn't work

```
$ wasmtime run sqlite_cli_rust.wasm
Error: component imports instance `sqlite:wasm/extension-loader@0.1.0`,
       but a matching implementation was not found in the linker
```

`wasmtime run` only provides WASI imports. This component imports
the `extension-loader`, `dispatch`, slot, and `zip-operations`
interfaces that only `sqlite-wasm-run` knows how to satisfy. Run it
through that binary instead:

```
$ sqlite-wasm-run --reactor sqlite_cli_rust.wasm

# Or with a file-backed db (needed for in-WASM spi.execute):
$ sqlite-wasm-run --reactor --db /tmp/my.db sqlite_cli_rust.wasm
```

## Building

The `bundled` feature of rusqlite compiles `sqlite3.c` from C via
the `cc` crate. Targeting `wasm32-wasip1` requires the `cc` crate
to use wasi-sdk's clang:

```sh
CC_wasm32_wasip1=$WASI_SDK/bin/clang \
AR_wasm32_wasip1=$WASI_SDK/bin/ar \
CFLAGS_wasm32_wasip1="--sysroot=$WASI_SDK/share/wasi-sysroot --target=wasm32-wasip1" \
  cargo component build --release
```

The result is `target/wasm32-wasip1/release/sqlite_cli_rust.wasm`.

## Source layout

- `src/lib.rs` — all Guest trait impls (cli, low-level, high-level,
  spi, logging, config). Plus `do_load`, `do_unload`, `do_open` for
  the dot-commands that touch the extension-loader host import.
- `src/dot.rs` — pure-text dot-command dispatcher (no host calls).
- `src/format.rs` — output formatter for the 8 modes.
- `src/settings.rs` — per-session CLI state (mode, headers,
  prompts, …).
- `src/state.rs` — low-level rusqlite handle table (db handle →
  Connection; stmt handle → SQL string + bindings + current row).
