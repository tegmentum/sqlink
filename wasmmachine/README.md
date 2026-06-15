# sqlite-cli as a wasmMachine application

Per PLAN-wasmmachine.md. Authors a v86-compatible machine spec
that wraps `sqlite_cli.component.wasm`, so the cli can be
instantiated identically by anything wasmMachine can dispatch
(local wasmtime, ssh-target hosts, ...).

## Files

- `sqlite-cli.json.template` — hand-authored spec skeleton, with
  `@@CLI_DIGEST_ARRAY@@` + `@@CLI_PATH@@` placeholders. Mirrors
  the structure of `~/git/v86/plans/python-v86.json`.
- `build-spec.sh` — builds the cli, hashes the component with
  blake3 (uses `b3sum` or `python3 -c 'import blake3'`),
  substitutes into the template, writes `sqlite-cli.json`.
- `sqlite-cli.json` — generated output. Committed-empty;
  regenerate with `make wasmmachine-build`.

## Build

```sh
make wasmmachine-build
# -> wasmmachine/sqlite-cli.json
# -> wasmmachine/sqlite_cli.component.wasm
```

## Seal + run (requires v86 tooling on PATH)

```sh
make wasmmachine-seal   # produce sealed identity
make wasmmachine-run    # instantiate locally
```

`wasmmachine` binary expected at `~/git/v86/target/release/`
(or on PATH some other way).

## What's NOT yet wired

The plan flagged 7 open v86-internals questions that block
deeper integration:

1. Which wasm engine wasmMachine uses to instantiate components
   (needs `crates/v86-component/src/` reading).
2. `wasmmachine seal` output format + identity stability.
3. Whether a wasmmachine build tool exists or every spec is
   hand-authored.
4. Tool / external-dependency surface format
   (`tools: [jq@1.7.1, ...]` from older README may be obsolete).
5. WASI provider identity (`provider_id: "wasi-host"` in the
   spec — is that wasmMachine's actual convention?).
6. SQLite extension provider IDs — the spec references
   `sqlite-extension-host` as a placeholder; the real provider
   identity comes from the deployment.
7. Integration test path — `wasmmachine run --check` or
   equivalent for asserting "the cli started and printed the
   banner."

These don't block the build pipeline shipping in this commit,
but they do block ending-to-end "wasmmachine run sqlite-cli.json"
working out of the box. Resolution requires reading v86
internals + producing answers documented next to this README.
