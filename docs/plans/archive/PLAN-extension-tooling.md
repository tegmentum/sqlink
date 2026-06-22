# Plan: extension porting tooling

> Distill the patterns observed across the 25+ extensions shipped
> in the recent batch into reusable tooling that halves the per-
> extension authoring time and prevents re-discovering compat
> issues.

## Goal

Reduce per-extension overhead from "100-200 LOC of mechanical
scaffolding + ad-hoc smoke testing + manually-tracked compat
gotchas" to a single command that scaffolds, builds, smokes,
and registers in provenance. The pattern is consistent enough
that 60-70% of every `lib.rs` is identical boilerplate; the
remaining 30-40% is "what does each scalar actually do."

Driver: the long-tail items (shapefile, DICOM, GTFS, MaxMind
GeoIP, etc.) are individually moderate-effort but the mechanical
overhead currently dominates. Cutting that overhead unlocks the
"port 5 in an afternoon" cadence for the simpler ones.

## What's in scope

### T1  Scaffold generator (`tooling/scaffold.py`)

A script that creates a working extension skeleton from a
template:

```bash
$ tooling/scaffold.py <name> [--crate <upstream-crate-name>]
Created extensions/<name>/Cargo.toml
Created extensions/<name>/src/lib.rs
Created extensions/<name>/smoke.sql
```

Inputs:
- `<name>` — extension name (lowercase, no underscores; becomes the
  Manifest name + the crate name suffix)
- `--crate` — optional upstream crate to wire into Cargo.toml deps

Outputs:
- `extensions/<name>/Cargo.toml` with wit-bindgen + the chosen crate
- `extensions/<name>/src/lib.rs` with:
  - The standard `wit_bindgen::generate!` invocation
  - The `Ext` struct + `MetadataGuest` + `ScalarFunctionGuest` impls
  - The `arg_text` / `arg_int` / `arg_blob` helper fns
  - A single placeholder scalar so it builds clean
- `extensions/<name>/smoke.sql` with the standard `.load` line +
  one placeholder `SELECT`

Templates live in `tooling/templates/`. Variables are simple
`{NAME}` / `{CRATE}` substitutions  no Jinja, just `str.format()`.

Validation: after scaffolding, run `cargo check --target
wasm32-wasip2` to confirm the skeleton compiles before the
user even starts editing.

### T2  Compatibility registry (`tooling/compat-registry.json`)

A JSON file tracking which upstream crates we've evaluated and
their wasm32-wasip2 status. Schema:

```json
{
  "crates": {
    "<crate-name>": {
      "status": "clean" | "needs-bootstrap" | "broken" | "abandoned",
      "version_tested": "x.y.z",
      "notes": "free-form explanation",
      "alternates": ["fallback-crate-1", "fallback-crate-2"]
    }
  }
}
```

Seed with the gotchas already learned this session:
- `zxcvbn` 3.x and 2.x  broken (wasm-bindgen for time)
- `typed-path` 0.12  needs RUSTC_BOOTSTRAP=1
- `tract-onnx` 0.23  needs RUSTC_BOOTSTRAP=1; 23 MB component
- `base62` 2.x  API is u128-only, not bytes-friendly
- `htmd` 0.5  clean
- `email_address` 0.2  clean
- `phonenumber` 0.3  clean but 2.6 MB component
- ... (~15-20 entries from this run)

The scaffold generator (T1) checks the registry when wiring
the user's `--crate` argument. If the crate is `broken`, warn
loudly with the reason; if `needs-bootstrap`, add a comment
to the generated Cargo.toml; if `clean`, just proceed.

### T3  Build pipeline one-shot (`make ext NAME=<n>`)

Makefile target consolidating the post-scaffold steps:

```bash
$ make ext NAME=detect
  cargo build --release --target wasm32-wasip2 -p detect-extension
  wasm-tools component new ... --adapt ...preview1.reactor.wasm
  python3 provenance/scan.py
  python3 tooling/smoke.py detect
```

Replaces the current 4-step manual sequence. Same target works
for newly-scaffolded and existing extensions.

### T4  Smoke harness (`tooling/smoke.py`)

Convention: each extension owns `extensions/<name>/smoke.sql`,
which the scaffold generator creates. The harness:

```bash
$ tooling/smoke.py detect      # smokes one extension
$ tooling/smoke.py --all       # smokes every ext that has a smoke.sql
```

Runs each `smoke.sql` against the cli with the extension auto-
loaded (`.load` line is in the file). Surfaces stdout + any
errors. No assertions in v1  the harness is a "did anything
panic" check, not a regression suite. Failures are advisory
only.

Useful for the catalog rebuild scenario noted earlier: after
an ABI-breaking WIT change, run `smoke.py --all` to detect
every extension that needs re-building.

### T5  Plan-doc row appender (`tooling/plan-add.py`)

Tiny appender for `PLAN-sqlite-plugins.md`'s function table:

```bash
$ tooling/plan-add.py detect 5 "slug/lang/mime detection"
```

Appends:
```
| detect (slug/lang/mime)         |    +5  | extensions/detect                  |
```

Auto-aligns columns. Stops the per-extension `Edit` ceremony.

### T6  Claude skill (`.claude/commands/new-extension.md`)

A project-level slash command that wraps T1-T5 into one
invocation:

```
/new-extension detect --crate slug,whatlang,infer
```

The skill file describes the command surface in the
Anthropic-skill convention (frontmatter + body). When the user
or Claude types the command, the skill runs:

1. `tooling/scaffold.py detect --crate slug,whatlang,infer`
2. Prompts Claude to edit `src/lib.rs` to add the actual scalars
3. After edits, `make ext NAME=detect` to build + smoke + scan
4. Suggests the commit message based on the changes

Acceptance test: scaffold + ship `mailto` extension using only
the skill command and one round of edits.

## What's out of scope (intentional)

### T7  `#[scalar]` proc-macro  DEFERRED

A proc-macro that eliminates the FID const + Manifest entry +
dispatch match-arm boilerplate would save another ~50 LOC per
extension. But:

- It's a proper crate's worth of work (~150-300 LOC of macro
  with synQuote machinery)
- The arg-coercion lowering is non-trivial (need to handle
  `String` / `i64` / `Vec<u8>` / `Option<T>` / `Result<T,
  String>` cleanly, plus optional defaults)
- The current `arg_text` / `arg_int` / `arg_blob` helpers are
  already a 3-line block at the top of every `call()`  not
  the biggest source of friction

Revisit after shipping 10+ more extensions on the T1-T6
tooling. If the boilerplate per-extension is still the
dominant cost, that's the signal to invest in the macro.

### Workspace / shared build artifacts

Each extension is its own wasm component with its own pinned
wit-bindgen output. Sharing build artifacts across extensions
sounds appealing but would complicate the per-extension
crate-type setup. Skip.

## Sequencing

1. T2 (compat registry  seed the data first; pure-data work)
2. T1 (scaffold generator  consumes T2)
3. T4 (smoke harness  consumes the `smoke.sql` convention T1
   establishes)
4. T3 (Makefile target  wraps T1 output + builds + T4)
5. T5 (plan-doc appender  trivial; do last)
6. T6 (Claude skill  composes T1-T5)

T1 + T2 are the bulk of the value. If we ship those alone, the
per-extension authoring drops noticeably.

## Estimated effort

| Item | Estimate |
|---|---|
| T1 scaffold generator | 1-2 hr |
| T2 compat registry (seed) | 30 min |
| T3 Makefile target | 15 min |
| T4 smoke harness | 1 hr |
| T5 plan-doc appender | 15 min |
| T6 Claude skill file | 30 min |
| **Total** | **~4-5 hr** |

After landing all six, the per-extension authoring time should
drop from ~30-60 min (with grep + lookups + manual smoke) to
~10-20 min (with scaffold + targeted edits + `make ext`).

## Implementation now

Shipping T1 + T2 + T3 + T4 + T6 in the next commit; T5 is
trivial enough to inline. T7 deferred per above.
