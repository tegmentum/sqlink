# extension-smoke

Per-extension smoke matrix for sqlink scenario 2 (the standalone
SQLite-in-wasm + cli + loader path). Task #395 in the release-
readiness sequence.

## What this does

For every loadable extension catalogued in
`provenance/extensions.db`, run one short SQL probe per surface
kind (scalar / aggregate / vtab / collation) and assert the
output. The probe loads the extension's `.component.wasm` via
sqlink's `.load`, executes the SQL, captures stdout, matches
against an expected literal or regex.

## Run

Prerequisites: native sqlink + the cli component must be built.

```sh
cargo build --release                                              # produces target/release/sqlink
cargo build -p sqlite-cli --target wasm32-wasip2 --release         # cli wasm
wasm-tools component new \
    target/wasm32-wasip2/release/sqlite_cli.wasm \
    -o target/wasm32-wasip2/release/sqlite_cli.component.wasm
```

Then:

```sh
# Test harness (parallel; one #[test] per plugin):
cargo test -p extension-smoke --release -- --nocapture --test-threads=8

# Sequential reporter (writes target/extension-smoke-report.md):
cargo run -p extension-smoke --release --bin extension-smoke-report
```

`SMOKE_DEBUG=1` dumps the full stdout/stderr per probe.

A test self-skips with a clear message if the extension's
`.component.wasm` file is missing  fresh clones run cleanly
even without every extension built.

## Fixtures

`fixtures.toml` is the source of truth for probes. Two ways to
populate it:

1. **Hand-roll** (preferred for validators, parsers,
   aggregates, vtabs): edit `gen-fixtures.py`'s `HANDROLLED`
   block. Re-run `python3 gen-fixtures.py > fixtures.toml`.
2. **Auto-derive** from `inventory.json`: `gen-fixtures.py`
   picks one scalar per plugin and emits a "this dispatches
   without erroring" probe. Catches LOAD-failed regressions
   even without precise output assertions.

Per-extension structure:

```toml
[extension.sha3.scalar]
sql = "SELECT sha3_256('test')"
expects = "36f028580bb02cc8272a9a020f4200e346e276ae664e45ee80745574e2f5ab80"

[extension.stats.aggregate]
setup = [
    "CREATE TABLE t(x REAL)",
    "INSERT INTO t VALUES (1.0), (2.0), (3.0), (4.0), (5.0)",
]
sql = "SELECT stddev_samp(x) FROM t"
expects_regex = "^1\\.5811"
```

## Known state at first commit

  - **1 PASS, 181 LOAD FAILED, 14 NO FIXTURE, 23 NO ARTIFACT,
    4 INTENTIONAL SKIP** as of the framework's first run.
  - The 181 LOAD FAILED bucket is almost entirely **stale
    `.component.wasm` artifacts** built against the previous WIT
    package name (`sqlite:wasm`) before the
    sqlite-lib-extraction refactor renamed it to
    `sqlink:wasm`. Each component needs `cargo build --target
    wasm32-wasip2 --release` followed by `wasm-tools component
    new`  shipped as a separate workstream.
  - This crate doesn't auto-rebuild extensions (that's a
    separate concern). Once a clean rebuild pass lands, this
    matrix should report mostly PASS.

## Adding fixtures for a new extension

1. Build the extension's `.component.wasm`.
2. Look at its `Manifest::describe()` in
   `extensions/<plugin>/src/lib.rs` to find the function names
   and arities.
3. Add a fixture under HANDROLLED in `gen-fixtures.py`. Pick a
   simple input and a deterministic expected output.
4. Regenerate `fixtures.toml`:
   `python3 gen-fixtures.py > fixtures.toml`.
5. `cargo test -p extension-smoke --release -- --nocapture
   smoke_<plugin>` to spot-check.

## Scenarios 1 + 3 (deferred)

The fixture data here is reusable. Scenario 1 (native sqlink
loader) and scenario 3 (browser runtime) will eventually run
the same probes against their own host shapes; only the harness
changes, not the SQL. See task #397 / #398.
