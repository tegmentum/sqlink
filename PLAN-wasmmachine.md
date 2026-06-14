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

## Skim findings (2026-06-14)

Confirmed from `~/git/v86/`:

- **Spec format is JSON**, with two layered shapes:
  - **Machine-level** (`~/git/v86/README.md` hello-machine example):
    `schema_version`, `name`, `capability_profile`,
    `kernel_ref` (with `digest: "blake3:..."`, `size`,
    `media_type`, `sources`). Used for `wasmmachine run
    hello.json`.
  - **Component-composition** (`~/git/v86/plans/python-v86.json`):
    `version`, `root`, `components` (each with `id` + digest
    as int array), `bindings` (consumer/import_name/provider/
    export_name  WIT-level wiring across components),
    `secrets`, `policy`.
  - For sqlite-cli we want the composition format: one
    component (cli) plus a provider component supplying WASI
    imports, wired via `bindings`.
- **ArtifactRef shape** (from
  `~/git/v86/docs/machine-storage-model.md`): standardised
  digest + size + media_type + transport list (`local`, `s3`,
  `https`, `iroh`, `ipfs`, `oci`). Our cli component goes here
  as `media_type: application/wasm` with a `local:` source
  pointing at our build output.
- **No `wasmtime` dependency anywhere in v86 crates.** v86 has
  its own runtime story under `crates/v86-core` and
  `crates/v86-component`. The hosting-substrate doctrine says
  "instances are admitted and controlled by a portable
  WebAssembly web service"  but the actual instantiation
  runtime (which wasm engine executes the kernel + components)
  isn't surfaced in the doc skim and needs deeper code reading
  before commit.
- **Capabilities** appear to be flexible per-spec: the
  `capability_profile` block in machine spec is open-ended
  (trust_mode, filesystem, network, memory_bytes, cpus); the
  composition `policy` block carries arbitrary `capabilities`
  fields. We'd declare what we need (filesystem access for db
  files, etc.) without a curated taxonomy in the way.

## Implications for sqlite-cli's spec

```json
{
  "version": "1",
  "root": "sqlite-cli",
  "components": [
    { "id": "sqlite-cli", "digest": "blake3:<hash-of-cli-component-wasm>" },
    { "id": "wasi-host",  "digest": "blake3:<wasmMachine-provided-wasi-provider>" }
  ],
  "bindings": [
    { "consumer_id": "sqlite-cli", "import_name": "wasi:cli/run@0.2.4",      "provider_id": "wasi-host", "export_name": "wasi:cli/run@0.2.4"      },
    { "consumer_id": "sqlite-cli", "import_name": "wasi:filesystem/types@0.2.4", "provider_id": "wasi-host", "export_name": "wasi:filesystem/types@0.2.4" }
    // ... etc for each WASI import
  ],
  "secrets": [],
  "policy": {
    "determinism": "relaxed",
    "capabilities": ["filesystem:writable:/db"]
  }
}
```

After the Plan 3 substrate switch (tvm-guest-mm, no host
imports), the only external imports are WASI  no `tvm-wasmtime`
binding needed. That's a clean spec.

## Open questions remaining

1. **Component runtime**  which wasm engine does wasmMachine
   use to instantiate components? Needs reading
   `crates/v86-component/src/` to confirm. Must support multi-
   memory components if Plan 3's substrate switch is in flight.
2. **Sealing pipeline**  `wasmmachine seal machine.json` is
   mentioned in the README; what does it produce, and is the
   sealed identity stable across re-builds of the same source?
3. **Build pipeline integration**  is there a wasmmachine
   build tool that takes wasm + spec template and emits a
   sealed spec, or hand-author every time?
4. **Tools / external dependencies**  the README mentioned
   `tools: [jq@1.7.1, ...]` but I didn't see this in either
   spec example. May be an obsolete README pattern or in a
   different layer.

These don't block architectural commitment but block detailed
implementation. Answer when we actively start Plan 4.

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
