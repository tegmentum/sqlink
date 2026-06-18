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

### 3. Identify the shape, then edit `extensions/<name>/src/lib.rs`

BEFORE writing code, read `tooling/extension-patterns.md` to find
which of the 10 documented shapes this extension matches:

  classifier / validator+extractor / parser-union / alias-table /
  exact-key lookup (+ auto-detect variant) / formatter+parser pair /
  pure formatter / coord transform / base-N algorithm / tokenize-
  then-compare

The shape determines the entire skeleton. Picking right at this
step saves significant refactoring. If the new extension doesn't
fit a documented shape, plan to ADD it to extension-patterns.md
in your lessons-learned entry.

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

NULL results render as `<NULL>` in the harness output (T-19 sentinel).
Write `<NULL>` literally in smoke.expected.

Common gotchas in smoke.sql output  see `tooling/cli-cheatsheet.md`
under "Harness output limitations" (leading whitespace eaten by
prompt regex, integer-valued reals drop `.00`, etc.).

### 5. Build + smoke + seed

```bash
make ext NAME=<name>
```

If the compat-registry flagged the crate as `needs-bootstrap`, run:

```bash
make ext NAME=<name> BOOTSTRAP=1
```

`make ext` does: cargo build, wasm-tools component new with the
wasi adapter, provenance scan, single-extension smoke. Output
ends with PASS/FAIL.

Once the outputs are right, seed the assertion file:

```bash
python3 tooling/smoke.py --seed-expected <name>
```

Writes `extensions/<name>/smoke.expected` from the current parsed
output with a `# AUTO-SEEDED  review and trim` banner. Replace
the banner with a short, real description of what the smoke covers
BEFORE committing. The banner is the guard against shipping
unreviewed assertions  treat it as a TODO marker.

### 6. Update plan doc

```bash
python3 tooling/plan-add.py <name> <scalar-count> "<short-desc>"
```

Appends a row to `PLAN-sqlite-plugins.md`.

### 7. Lessons-learned entry

Generate the stub with today's date pre-filled:

```bash
python3 tooling/lessons-stub.py <name>            # plugin
python3 tooling/lessons-stub.py --kind investigation T-NN "scope"
```

Pipes a paste-ready section to stdout. Append to
`tooling/lessons-learned.md` and fill in the bullets. The
template enforces the four-section shape:

  **What I built:** signatures + one-line algorithm summary
  **What worked:** patterns that helped, esp. from prior ships
  **What surprised me:** wrong-shape choices, subtle bugs
  **Tooling opportunity:** (T-NN new) or (none new)

Where this lives in the depth gradient:
  - lessons-learned.md  one-liner WHY-NOT for this ship
  - extension-patterns.md  shape this matched (or new shape)
  - snippets/README.md  paste-and-own code if extracted

### 8. End-of-ship regression check

```bash
make ext-ship NAME=<name>
```

`ext-ship` bundles `make ext` with a full `smoke --all -j 0`
regression pass (~15s parallel). Use this instead of bare
`make ext` for the FINAL pre-commit run  it catches regressions
in unrelated extensions that the single-extension smoke can't
see. This discipline caught the T-17 parallel-flake bug
mid-session (commit 98470b4); the cost of skipping it is real.

### 9. Commit

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

If a non-extension change might have broken the catalog (e.g. an
ABI-breaking WIT change, a host-side refactor):

```bash
make ext-smoke-all   # equivalent to: tooling/smoke.py --all -j 0
```

Walks every `extensions/*/smoke.sql` and reports PASS/FAIL.
Asserted smokes (those with `smoke.expected`) diff against the
expected output; unasserted ones just check for panics / load
failures. Parallel at -j 0 = cpu_count workers (~15s on 8 cores).

## Status checks

```bash
python3 tooling/t-status.py         # T-* open vs closed report
python3 tooling/smoke.py --list     # what's smoked + asserted
```
