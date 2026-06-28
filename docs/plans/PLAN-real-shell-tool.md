# PLAN: the real sqlite3 shell as a wasm run-tool (Route A)

The sqlink mirror of ducklink's Phase 1 + Route A: ship the GENUINE
upstream sqlite3 shell (`deps/sqlite/shell.c`) as a wasm component run
through the sqlink host, replacing the hand-rolled CLI port (the
"lookalike"), and bridge its `.load` to the host's wasm-component
extension loader.

## P1 — real shell as a wasm tool  (LANDED, verified)

- `scripts/build-shell-wasm.sh` compiles `deps/sqlite/shell.c` +
  `sqlite3.c` + `scripts/shell-wasi-shims.c` against the wasi-sdk to a
  `wasm32-wasip2` `wasi:cli/run` COMPONENT
  (`target/wasm32-wasip2/release/sqlite3-shell.component.wasm`).
  - No linenoise/readline/editline (no termios on wasip2) -> the
    shell's built-in `local_getline()` fallback line reader. Reads
    stdin via WASI; interactive + piped input both work.
  - Signals / process-clocks / getpid: the wasi-sdk emulation libs
    (`-lwasi-emulated-{signal,process-clocks,getpid}`). `system()` (the
    `.shell`/`.system` dot-commands) has no emulation lib, so
    `scripts/shell-wasi-shims.c` stubs it to ENOSYS.
  - wasi-sdk 33 (clang 22) emits the component DIRECTLY for the
    wasm32-wasip2 target — no `wasm-tools component new` + adapter step.
- `sqlink run-tool <component> [--db PATH] [-- args]` (host/src/main.rs):
  a thin alias over the existing positional run path, which already
  instantiates a `wasi:cli/run` component with inherited stdio (a real
  TTY) and the full host-import surface (extension-loader / dispatch /
  spi / spi-loader). Single run code path.
- `wasmmachine/build-spec.sh` + `sqlite-cli.json.template` repointed at
  the real-shell component; tool runtime = the sqlink host.

Verified: `sqlink run-tool` runs sqlite3 3.53.1 with genuine `.mode
box`/`.mode json` rendering and the full 61-command `.help`
(`.excel`/`.expert`/`.import`/`.recover`/...) — the real shell, not the
lookalike.

## P2 — bridge `.load` to the host wasm-component loader  (DESIGN + precise remaining step)

### The crux (root cause)

`deps/sqlite/sqlite3.h` force-defines `SQLITE_OMIT_LOAD_EXTENSION` under
`#if defined(__wasi__)` (sqlite3.h ~line 11353-11358). That omits BOTH
the library's `sqlite3_load_extension` C-API AND the shell's `.load`
dot-command (gated at `shell.c` ~line 33553 by
`!defined(SQLITE_OMIT_LOAD_EXTENSION)`). So in the current P1 build,
`.load` reports "unknown command". This is upstream-intentional: WASI
has no dlopen-able shared libraries.

We do NOT want dlopen anyway — Route A makes `.load` resolve a
`sqlite:extension` WASM COMPONENT via the host loader, not a `.so`.

### The mechanism already exists in this repo (in C)

The bridge — `.load` -> host WIT loader -> trampolines on the shell's
OWN sqlite3 connection — is ALREADY implemented in C and works against
a statically-linked sqlite3:

- `src/cli/sqlite_cli.c` (the C lookalike CLI) `.load` handler
  (~line 538-606, under `#ifdef SQLITE_WASM_UNIFIED`):
  1. `sqlite_wasm_extension_loader_load_extension(&path, &opts,
     &manifest, &err)` — the `sqlite:wasm/extension-loader` WIT import;
     the host reads the component, policy-gates it, returns a manifest.
  2. `wasm_register_dynamic_manifest(state->db, ext_name, &manifest)` —
     installs scalar/aggregate/collation/hook/vtab TRAMPOLINES on the
     CLI's own `sqlite3*` (`state->db`).
- `src/exports/extension-unified.c` — `wasm_register_dynamic_manifest`
  + the `wasm_dyn_xfunc` trampoline: `sqlite3_create_function_v2(db,
  name, nargs, ..., wasm_dyn_xfunc)` where `wasm_dyn_xfunc` marshals
  `sqlite3_value*` -> WIT `sql-value`, calls the
  `sqlite:wasm/dispatch.scalar-call(ext-name, func-id, args)` import,
  and marshals the result back. `.unload` tears the trampolines down
  via `wasm_unregister_dynamic_manifest`.
- `src/bindings-unified/sqlite_cli_unified.{c,h}` — the generated
  wit-bindgen-c bindings for extension-loader / dispatch / spi-loader.

The host side is ALREADY wired: `host/src/main.rs`'s run path adds
extension-loader, dispatch, spi, and spi-loader to the linker for ANY
`wasi:cli/run` component it runs. So a shell component that imports
those interfaces would have them satisfied by the same `HostWrap`
the lookalike uses.

### Precise remaining step (the one grafting pass)

Graft the working `.load` glue from `src/cli/sqlite_cli.c` +
`src/exports/extension-unified.c` onto the real `shell.c`:

1. Re-enable `.load` in the shell build. `shell.c`'s
   `SQLITE_CUSTOM_INCLUDE` hook (line 88) runs BEFORE `sqlite3.h`, so
   it cannot undo the omit. Use a build-time patched COPY of `shell.c`
   (keep `deps/sqlite/shell.c` pristine — it is a vendored amalgamation):
   `build-shell-wasm.sh` seds in `#undef SQLITE_OMIT_LOAD_EXTENSION`
   immediately before the `.load` gate (~line 33552), and replaces the
   `sqlite3_load_extension(p->db, zFile, zProc, &zErrMsg)` call body
   (line 33567) with a call into the graft glue (below). Alternative:
   add a small custom dot-command via a shell.c patch — the sed-replace
   of the existing `.load` body is the least surface.
2. Add a `scripts/shell-load-glue.c` that provides the graft entry
   point — essentially `wasm_register_dynamic_manifest` adapted to take
   the shell's `p->db`: call
   `sqlite_wasm_extension_loader_load_extension`, then
   `sqlite3_create_function_v2` the manifest's scalars (+ aggregates /
   collations / hooks / vtabs) onto `p->db` with the `wasm_dyn_xfunc`
   trampoline. Reuse `src/exports/extension-unified.c` verbatim if its
   bindings match the current WIT; otherwise regenerate.
3. Regenerate the wit-bindgen-c bindings against the CURRENT WIT. Two
   reasons the committed `src/bindings-unified/*` cannot be reused as-is:
   (a) NAMESPACE SKEW — the host now wires `sqlink:wasm/dispatch` and
   `sqlink:wasm/extension-loader` (the `sqlink:` namespace, see
   `host/src/main.rs` `bindings::sqlink::wasm::{dispatch,extension_loader}
   ::add_to_linker`), whereas the legacy C bindings import
   `sqlite:wasm/dispatch` / `sqlite:wasm/extension-loader`. The shell's
   import names must match the host's exactly or the linker will not
   satisfy them. (b) the working tree is mid-migration to
   `sqlite:extension@1.0.0`; the committed bindings target `@0.1.0`.
   Regenerate once the `@1.0.0` migration in `sqlite-loader-wit` settles
   — this skew is the concrete reason P2 is a separate pass.
   Generate with `wit-bindgen c` against `sqlite-loader-wit/wit` for the
   shell's import set (extension-loader + dispatch + types [+ spi-loader
   if the shell ever calls register-* directly; for scalars the
   trampoline only needs `dispatch`]).
4. Build: `build-shell-wasm.sh` compiles patched-shell.c + sqlite3.c +
   shell-load-glue.c + extension-unified.c + the generated bindings ->
   the component now IMPORTS `sqlite:wasm/extension-loader`,
   `sqlite:wasm/dispatch`, `sqlite:extension/types`. Those are already
   wired in the host run path.

### P2 verification (when landed)

In the real shell via `sqlink run-tool`:
`.load <a sqlite:extension scalar component>` then a query using its
function -> correct result, dispatched shell -> host loader ->
trampoline on the shell's conn -> `dispatch.scalar-call` -> the wasm
component. Use an existing scalar `sqlite:extension` from the catalog
(e.g. `extensions/sha1` / `extensions/case` / `extensions/roman`).

## P4 — v86 execd runtime-selection + interactive PTY  (LANDED, verified)

The real sqlite3 shell now runs as a FULLY INTERACTIVE console through the
v86 WasmMachine, at parity with ducklink's duckdb console.

### Generic vs sqlink-specific (no re-port needed)

ducklink's interactive-pty work is GENERIC and already lives in v86
(`crates/wasmmachine-execd`): `pty.rs` (openpty + slave-as-stdio,
cooked-mode line discipline), the interactive cell path in `cell.rs`
(master <-> cell stream protocol), `ExecConfig.interactive`, and the
`POST /v1/cells/{id}/input` + `GET /v1/cells/{id}/output` API. That
machinery runs ANY `wasi:cli/run` tool over a pty — so P4 was WIRING,
not a re-port of `pty.rs`.

The sqlink-specific bits (all in v86 branch `feat/execd-pty-sqlite`):

- `cell.rs`: a `"sqlink"` runtime branch alongside `"ducklink"` ->
  spawns `sqlink run-tool <component> [--db PATH]` (the P1 entry point,
  which inherits stdio = a real TTY and wires the full host-import
  surface for the future `.load`, P2). The shell links sqlite3
  statically, so `runtime = "wasmtime"` runs it directly too.
- `config.rs`: document the `"sqlink"` runtime + `SQLINK_PATH` + the
  `extensions_dir` (`--db`) reuse.
- `examples/tools/sqlite.tool.json`: cli-tool manifest
  (`console.interactive`, `/bin/sqlite3`) mirroring `duckdb.tool.json`.
- `examples/tools/execd-sqlite.toml`: interactive execd config
  (`interactive = true`, `runtime = "sqlink"`) mirroring
  `execd-duckdb.toml`.
- `cell.rs` test `pty_interactive_repl_sqlite` (ignored, opt-in).

sqlink ships its own copies for discoverability next to the shell build:
`wasmmachine/sqlite.tool.json` + `wasmmachine/execd-sqlite.toml`.

### P4 verification

Through the v86 execd cell layer (pty, `runtime = "sqlink"` ->
`sqlink run-tool`): `sqlite>` prompt (isatty over the pty), a live
`SELECT 42 AS answer;` -> box result, `.mode box`, cooked-mode line
editing (`.tablex<bs><bs>es` -> `.tables`), `.quit` -> exit 0. Drive it
with `cargo test -p wasmmachine-execd pty_interactive_repl_sqlite --
--ignored --nocapture` (set `EXECD_PTY_SQLITE_SHELL` +
`EXECD_PTY_SQLINK`).

Caveat (same as the duckdb console): no termios/linenoise on wasip2, so
line editing is the pty cooked-mode line discipline (echo + backspace);
no in-app arrow-key history. `.load` extension dispatch is P2, not P4.

## P3 — follow-on (not in this run)

- P3: generalize the CLI resolver into the multi-provider resolver +
  native passthrough via `sqlink-native`.

## Reuse leveraged

- The existing host run path (extension-loader/dispatch/spi/spi-loader
  already wired for any `wasi:cli/run` component) — `run-tool` is a thin
  alias, no second linker-wiring copy.
- The vendored `deps/sqlite/{shell.c,sqlite3.c}` + the wasi-sdk
  toolchain sqlink already uses.
- For P2: the in-repo C `.load` bridge (`src/cli/sqlite_cli.c`,
  `src/exports/extension-unified.c`, `src/bindings-unified/*`) — the
  trampoline mechanism is solved in C; P2 is grafting it onto shell.c.
