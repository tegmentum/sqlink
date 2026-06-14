# Plan: Register sqlite-cli as a wasmMachine application

## Goal

Ship `sqlite-cli` as a sealed, content-addressed wasmMachine
application (`~/git/v86/`). `wasmmachine run sqlite-cli.json`
instantiates the cli identically on local wasmtime, ssh-target
hosts, or wherever wasmMachine can dispatch.

## wasmMachine recap

From `~/git/v86/README.md`:

> A WasmMachine is a sealed artifact graph: kernel, filesystem,
> tools, and capabilities  all content-addressed. The machine's
> identity is `digest(spec)`. It doesn't change when you move
> the machine to a different store, bundle it for transport, or
> deploy it to a different target.

```
MachineSpec
  kernel_ref:  blake3:a1b2c3...    (verified before run)
  rootfs_ref:  blake3:b2c3d4...    (verified before run)
  tools:       [jq@1.7.1, ...]     (capability-gated)
  profile:     1 CPU, 128MB, no network
```

## Out of scope: wasmMachine's artifact store

wasmMachine has its own content-addressed store for kernel /
rootfs / tool blobs, owned by the wasmMachine runtime with
lifecycle tied to machine identity. The SQLite extension CAS
cache (Plan 1) is a separate concern with a separate lifecycle,
and the two stores **do not share schema or storage**. They both
use blake3 because blake3 is the right primitive  that's the
only overlap.

## What the spec looks like

`wasmmachine/sqlite-cli.json` (or whatever format wasmMachine
accepts):

```json
{
  "name": "sqlite-cli",
  "version": "0.1.0",
  "kernel_ref": "blake3:<hash-of-cli-component-wasm>",
  "rootfs_ref": "blake3:<hash-of-rootfs>",
  "tools": [
    "tvm-guest-mm@<ver>",
    "wasi-p2@0.2.4"
  ],
  "profile": {
    "cpu": 1,
    "memory": "128MB",
    "network": "deny",
    "fs": { "writable": ["/db"] }
  },
  "capabilities": [
    "sqlite-vfs-tvm",
    "sqlite-pcache-tvm"
  ]
}
```

After last commit, the cli is multi-memory + tvm-guest-mm-based
(if Plan 3 ships first), so no `tvm-wasmtime` host capability is
needed. The cli runs against any multi-memory wasi-p2 engine.

## Concrete deliverables

- **`wasmmachine/sqlite-cli.json`**  the spec, hand-authored
- **`Makefile` target `make wasmmachine-build`**:
  1. `cargo build --release` (cli wasm)
  2. `wasm-tools component new` (component wrap)
  3. blake3 the result, write `kernel_ref` into spec
  4. Build minimal rootfs (probably empty or a single config
     file), hash, write `rootfs_ref`
  5. Run wasmMachine's seal step to compute `digest(spec)`
  6. Output: `sqlite-cli.json` (hashed + sealed) +
     `sqlite-cli.wasm` (kernel) + `sqlite-cli.rootfs.tar` or
     similar (rootfs)
- **`make wasmmachine-run`**  invokes `wasmmachine run
  wasmmachine/sqlite-cli.json` locally, asserts exit code
- **Integration test in `host/tests/`**  builds + runs the
  spec + asserts stdout contains expected SQLite version banner

## Open questions

The wasmMachine spec format is documented in `~/git/v86/`. The
following need a read-through of that codebase to answer
concretely:

1. **Spec format**  is it JSON, TOML, a typed Rust struct? Is
   it stable or evolving?
2. **Capability vocabulary**  are `sqlite-vfs-tvm` / 
   `sqlite-pcache-tvm` valid capability names, or are
   capabilities fixed to a wasmMachine-curated set? If the
   latter, we declare what we need via standard names.
3. **Tools mechanism**  what does `tools: [tvm-guest-mm@<ver>]`
   resolve to? A blob ref? An external dependency? With the
   tvm-guest-mm switch (Plan 3), no external TVM tool is
   needed; we should remove it from the spec entirely.
4. **Component instantiation**  does wasmMachine instantiate
   via its own embedded wasmtime, via jco, or via a different
   runtime? Affects whether the cli wasm we produce composes
   directly.
5. **Sealing / seal verification**  what's the step that
   converts an authored spec into a sealed one with verified
   refs?
6. **Build pipeline integration**  is there a wasmMachine
   build tool that takes wasm + rootfs and produces the sealed
   spec, or do we author it manually?
7. **Runtime story**  does `wasmmachine run` block until the
   machine exits, or fork it and return a handle?

These are research questions, not design ones  the answers
come from reading the v86 codebase. They block detailed spec
authoring but not the architectural commitment to ship.

## Order of operations

1. **Read v86 docs**  answer the seven open questions
2. **Author minimal spec**  start with the simplest valid
   sqlite-cli machine; iterate
3. **Build pipeline**  Makefile targets that produce a sealed
   spec from a clean build
4. **Integration test**  local `wasmmachine run` + assert
5. **Doc**  README section showing the wasmmachine invocation
   pattern for end users
