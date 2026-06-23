# Composed cli+sqlite-lib component — import surface

Snapshot of `wasm-tools component wit target/wasm32-wasip2/release/cli_with_sqlite.component.wasm`
after `INITIAL_PAGES=4096 ./scripts/build-composed-runtime.sh`.
Component size: ~4.2 MB.

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

## Open blocker — jco multi-memory

```
jco transpile cli_with_sqlite.component.wasm --instantiation async
  → ComponentError: unsupported section found in module using
    multiple memories
```

The composed component contains multi-memory core modules (4096
8192 pages × 4 memories per the inner sqlite-lib, plus the cli's
own memory). `wasm-tools print --skeleton` shows:

  (memory (;0;) 4096 8192)
  (memory (;1;) 4096 8192)
  (memory (;2;) 4096 8192)
  (memory (;3;) 4096 8192)
  (memory (;0;) 46)         ; the cli's own memory

`@bytecodealliance/jco-transpile` does not currently support
multi-memory. This blocks the entire Stage 8 transpile + browser
load pipeline.

Possible paths forward (none implemented in this stage):

  1. Build sqlite-lib in single-memory mode for the browser
     target (drop tvm multi-memory cold-tier in this build flavor).
     The CLI smoke under wasmtime would continue using the multi-
     memory build.
  2. Upstream multi-memory support in jco / js-component-bindgen.
     This is a real ask, not a quick fix.
  3. Use Wasmtime's `--allow-multi-memory` in the browser via
     a different transpile (e.g. wasm-bindgen / wamr) — major
     scope change.

Recommend #1 for the next attempt. A `sqlite-lib-single-memory`
build flavor would let the composed component target the browser
without losing the multi-memory cold-tier on native runtimes.
