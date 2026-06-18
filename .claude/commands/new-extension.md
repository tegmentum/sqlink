---
description: Scaffold + iterate on a new SQLite-wasm extension. Wraps tooling/scaffold.py + make ext + tooling/smoke.py.
---

# /new-extension — scaffold a SQLite-wasm extension

When the user invokes this command, you are scaffolding and porting
a new extension into `extensions/<name>/`. The repo has standard
tooling for this  use it instead of copy-pasting an old extension.

## Input format

```
/new-extension <name> [--crate crate1,crate2,...] [--description "..."]
```

Examples:
- `/new-extension mailto` — scaffold an empty extension named `mailto`
- `/new-extension mailto --crate url` — scaffold with `url` crate wired into deps
- `/new-extension shapefile --crate shapefile --description "GIS .shp reader"` — full

## Procedure

Do these in order. Don't skip steps; the tooling enforces the
conventions so the extension lands clean.

### 1. Probe the compat-registry first

If `--crate` was supplied, check `tooling/compat-registry.json` for
each entry. If any are marked `broken`, surface the reason to the
user and STOP  pick a different crate or abandon. If any are
`needs-bootstrap`, mention it so the user knows.

You can also run:
```bash
python3 tooling/scaffold.py --list-broken
```
to see all flagged crates at once.

### 2. Run the scaffold

```bash
python3 tooling/scaffold.py <name> --crate <comma-list> --description "<text>"
```

This:
- Creates `extensions/<name>/{Cargo.toml, src/lib.rs, smoke.sql}`
- Wires the requested crates into `[dependencies]`
- Annotates Cargo.toml with compat notes pulled from the registry
- Runs `cargo check --release --target wasm32-wasip2` to confirm
  the skeleton compiles BEFORE you start editing

If the build-check fails, fix the immediate issue (usually a
feature-flag tweak the registry knew about) before continuing.

### 3. Edit `extensions/<name>/src/lib.rs`

The scaffold ships with one placeholder scalar (`<name>_placeholder`).
Replace it with the real scalars. Pattern per scalar:

1. Add an FID const at the top
2. Add a `ScalarFunctionSpec` entry to the `scalar_functions` vec
3. Add a match arm in `ScalarFunctionGuest::call`

For deterministic scalars (the common case), use `FunctionFlags::DETERMINISTIC`.
For nondeterministic (random / time-of-call), use `FunctionFlags::empty()`.

The scaffold already includes `arg_text` / `arg_int` / `arg_blob`
helpers  use them in the match arms.

For ABI-stable return shapes:
- Errors on extract scalars  return `SqlValue::Null` (not `Err`)
- Errors on compare/validate scalars  return `SqlValue::Integer(0)`
- Hard errors (bad arg type, internal panic-class)  return `Err(...)`

### 4. Edit `extensions/<name>/smoke.sql`

Replace the placeholder `SELECT` with one representative query per
scalar. Cover:
- The happy path
- One edge case (empty input, NULL, out-of-range)
- One "fail-clean" case (invalid input that should NULL or 0,
  not panic)

### 5. Build + smoke

```bash
make ext NAME=<name>
```

If the compat-registry flagged the crate as `needs-bootstrap`, run:

```bash
make ext NAME=<name> BOOTSTRAP=1
```

`make ext` does the full sequence: cargo build, wasm-tools component
new with the wasi adapter, provenance scan, smoke run. Output ends
with PASS/FAIL.

### 6. Update plan doc

```bash
python3 tooling/plan-add.py <name> <scalar-count> "<short-desc>"
```

Appends a row to `PLAN-sqlite-plugins.md`.

### 7. Commit

Use the established commit-message shape: `feat(extensions): <name>
 <one-line summary>`, then a body that includes:
- Each scalar's signature
- A copy-pasted smoke transcript with the actual outputs
- Wasm size (KB or MB)
- Provenance plugin count

The body is for archeology  treat it as the answer to "what does
this extension do, what crate does it wrap, and how do I run it"
six months from now.

## When the scaffold isn't enough

Skip the scaffold and copy-edit an existing extension if:

- The new extension uses a non-`minimal` world (e.g. `tabular` for
  a vtab, `stateful` for an aggregate, `minimal-http` for an
  outbound-network scalar). The scaffold defaults to `minimal`;
  copy `extensions/parquet/src/lib.rs` (tabular) or
  `extensions/dns/src/lib.rs` (minimal-dns) as the template
  instead.
- The extension declares capabilities (http, dns, fs). Same as
  above  the scaffold ships with an empty `declared_capabilities`
  vec.

For the common case (a `minimal`-world scalar pack wrapping one
or two upstream crates), the scaffold path is faster than copy-
editing.

## Smoke-everything

If a recent change might have broken the catalog (e.g. an ABI-
breaking WIT change), regression-check every extension:

```bash
make ext-smoke-all
```

This walks every `extensions/*/smoke.sql` and reports PASS/FAIL.
Asserted smokes (those with `smoke.expected`) diff against the
expected output; unasserted ones just check for panics / load
failures.

## At end of every new-extension ship

```bash
make ext-ship NAME=<name>
```

`ext-ship` is the end-of-ship wrapper: `make ext` then
`smoke --all -j 0`. The full regression check is ~15s parallel
and catches anything the single-extension smoke missed. Use this
before committing a new plugin  not just `make ext`  so a
silent regression in an unrelated extension can't sneak in.
