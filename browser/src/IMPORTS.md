# Composed cli+sqlite-lib component — import surface

Two flavors live side-by-side:

* `cli_with_sqlite.component.wasm` — multi-memory build. Used by
  scenarios 1 + 2 under native wasmtime; 256 MiB-per-pool cold tier
  via tvm-mm-link. CANNOT be jco-transpiled today (see "blocker
  resolution" below).
* `cli_with_sqlite.single_memory.component.wasm` — single-memory
  build. Used by the browser; one inner linear memory total, jco
  transpiles it cleanly.

Snapshot below is of the single-memory flavor (component size
~4.3 MB) produced by `./scripts/build-composed-runtime-single-memory.sh`.

## Top-level world

```
import sqlite:extension/http@0.1.0
import sqlite:extension/policy@0.1.0
import sqlite:extension/types@0.1.0
import sqlite:extension/metadata@0.1.0
import sqlink:wasm/extension-loader@0.1.0
import sqlite:extension/spi-loader@0.1.0
import wasi:cli/environment@0.2.6
import wasi:cli/exit@0.2.6
import wasi:io/error@0.2.6
import wasi:io/poll@0.2.6
import wasi:io/streams@0.2.6
import wasi:cli/stdin@0.2.6
import wasi:cli/stdout@0.2.6
import wasi:cli/stderr@0.2.6
import wasi:cli/terminal-input@0.2.6
import wasi:cli/terminal-output@0.2.6
import wasi:cli/terminal-stdin@0.2.6
import wasi:cli/terminal-stdout@0.2.6
import wasi:cli/terminal-stderr@0.2.6
import wasi:clocks/monotonic-clock@0.2.6
import wasi:clocks/wall-clock@0.2.6
import wasi:filesystem/types@0.2.6
import wasi:filesystem/preopens@0.2.6
import wasi:random/insecure-seed@0.2.6
export wasi:cli/run@0.2.6
```

## Categorization

### WASI imports → @tegmentum/wasi-polyfill
All `wasi:*` imports are satisfied by the polyfill via plugins in
`@tegmentum/wasi-polyfill/wasip2/plugins/{random,clocks,io,filesystem,cli}`.

Note version: composed component targets `0.2.6`. The polyfill
exposes interfaces via `forInterfaces([...])` and registers
plugins; `jcoCompat: true` strips version suffixes from the keys
jco's --instantiation async emits.

### Sqlite-only-host imports → expected to be satisfied internally by sqlite-lib

These should be wired inside the composed component by the wac
recipe (`composition-cli-sqlite-lib.wac`). After composition they
appear in the WIT output as exports-of-the-inner-cli imported-by-
the-inner-sqlite-lib, NOT as imports the host has to satisfy.

  - sqlite:extension/http@0.1.0
  - sqlite:extension/policy@0.1.0
  - sqlite:extension/types@0.1.0
  - sqlite:extension/metadata@0.1.0
  - sqlite:extension/spi-loader@0.1.0

**STATUS WARNING**: these still appear as TOP-LEVEL imports of the
composed component. That likely means the wac recipe does NOT
fully internalize them — either by intent (the host can override)
or by oversight. If the host has to satisfy these too, the
runtime needs JS stubs for each (the cli at least calls into
spi-loader's register-* methods every time an extension loads).

### Host-implemented dynamic loader

  - sqlink:wasm/extension-loader@0.1.0

This is the "big one" — a ~30-method surface defined in
`wit/extension-loader.wit`. See `browser/src/extension-loader.js`
for the JS implementation skeleton (Task 8.3).

The cli calls into this for:
  - `.load PATH`               → load_extension(path, opts)
  - `.load FILE.wasm` (preload)→ load_extension_from_bytes(name, bytes, opts)
  - `.unload NAME`             → unload_extension(name)
  - `.cache stats / gc / ...`  → cache_*
  - `.run FILE.wasm`           → run_wasm / run_source
  - dot-command dispatch       → dispatch_dot_command(name, args, state)

For browser scenario 3, the smallest viable subset is:
  - load_extension_from_bytes  (the runtime loadExtension path)
  - extension_digest           (cli's grant-pin lookup)
  - list_extensions            (introspection)
  - is_extension_loaded        (existence check)
  - dispatch_dot_command       (only if dot-commands needed)
Everything else can return loader-error initially.

## Blocker resolution — single-memory flavor

Original blocker:
```
jco transpile cli_with_sqlite.component.wasm --instantiation async
  → ComponentError: unsupported section found in module using
    multiple memories
```

The composed multi-memory component contains 4 pool memories from
the inner sqlite-lib plus the cli's own default memory. jco does
not support multi-memory inner core modules.

Fix landed: sqlite-pcache-tvm + sqlite-vfs-tvm + sqlite-lib each
gained a `single-memory` Cargo feature that selects the in-proc
HashMap/Vec<u8> backends on wasm32 instead of the multi-memory
ones. The browser flavor (`build-composed-runtime-single-memory.sh`)
turns the feature on; the resulting cdylib has exactly ONE linear
memory and zero `tvm_mm` imports. `wasm-tools component new` wraps
it directly (no tvm-mm-link). jco transpile now succeeds.

Scenarios 1 + 2 keep using the multi-memory build because pool
capacity matters there. Both flavors live side-by-side in
`target/wasm32-wasip2/release/`.

## Open follow-up — REPL stdin/stdout marshalling under polyfill

The composed component instantiates cleanly under @tegmentum/wasi-
polyfill (verified manually). Calling its `wasi:cli/run` runs the
CLI's `embed_core_dotcmd()` (which logs 10 auto-load 404s to stderr
because no extensions are in the JS registry — expected today) and
then prints `sqlite> `. After one stdin chunk is read containing
both the user query and `.quit`, the CLI's REPL exits without
emitting the query result or a second prompt.

Repro: feed `SELECT 1+1;\n.quit\n` via a `QueueInputStream` to
the polyfill's stdin plugin; collect stdout; observe only the
first prompt and one `tryRead` call. Under native wasmtime the
same input produces `sqlite> 2\nsqlite> \n` correctly.

RESOLVED (Phase C persistent-session landing): the polyfill's
`WasiInputStreamWrapper.blockingRead` returns a synchronous empty
Uint8Array when the underlying queue is empty, which the wasip1
adapter reads as EOF — the cli exits on first idle. host-imports.js
now monkey-patches that wrapper so blockingRead awaits the impl's
async `read()`. Under JSPI the wasm caller suspends until the host
pushes the next exec()'s SQL into the QueueInputStream.

sql.js has been dropped and `openDatabase()` is now hard-wired to
the composed runtime. See sqlink-composed.js's ComposedDatabase
for the session lifecycle and sentinel-framed exec().
