<p align="center">
  <img src="extensions-site/sqlink_logo.png" alt="SQLink" width="320">
</p>

# SQLink

SQLite + a portable extension ecosystem, distributed as
[WebAssembly Components](https://component-model.bytecodealliance.org/).
The full SQLite C library compiles to WASI Preview 2 alongside a
cli, a host runtime, ~110 extension components, and a contract
(`sqlite-loader-wit`) that says how any wasm component becomes
an extension — scalar function, aggregate, collation, virtual
table, authorizer, or interactive dot command.

The point: you can write a SQLite extension in any language that
targets wasm (Rust, C, AssemblyScript, ...), publish it as a
`.wasm` file, and load it into the cli with `.load FILE` — the
same shape `sqlite3_load_extension()` has, but sandboxed,
portable, and language-agnostic.

## Three deployment scenarios

The same WIT-shaped `.wasm` extension runs against three
different SQLite hosts. The extension binary doesn't change; the
host does:

1. **Native SQLite + sqlink loader.** A traditional SQLite
   installation loaded as a system library, with a sqlink-shaped
   sqlite extension that embeds a wasm runtime (wasmtime, wamr,
   ...). `sqlite3_load_extension("sqlink_loader")` from any
   SQLite-linked program; subsequent `.load <ext>.wasm` calls
   bootstrap the wasm runtime, host the extension, and bridge
   its scalar / aggregate / vtab / hook surface back into the
   native SQLite connection. Lets existing native-SQLite
   deployments adopt the extension catalog without recompiling
   SQLite itself.

   Sub-option:
     - **`sqlink-native` standalone binary.** A reference loader
       that bundles libsqlite3-sys + the sqlink-host wasm runtime
       into a single executable. Same extension binaries as
       Scenario 2 (the wasm cli path), but SQLite itself runs
       natively  no `wasi:cli/run` component on the call stack
       so extensions calling `spi.execute` back into the host
       work without the recursive-entry constraint described in
       `host/SPI.md`. Build with `cargo build --release -p
       sqlink-native`; usage shape mirrors Scenario 2:

       ```bash
       printf '.load extensions/sha3/.../sha3_extension.component.wasm
       SELECT sha3('hello', 256);
       .exit
       ' | ./target/release/sqlink-native --db mydata.db
       ```

       The same `tests/extension-smoke` matrix runs against this
       binary via `cargo test -p extension-smoke --test
       extension_smoke_native --release` (Scenario 2's existing
       `extension_smoke` test still runs the wasm cli path
       unchanged).

2. **Standalone SQLite-in-wasm + extensions.** This repo's main
   path: the entire SQLite C library is compiled to wasm32-wasip2
   alongside the cli component, hosted by the `sqlink` native
   binary (a wasmtime-based runtime). `.load <ext>.wasm` loads
   another wasm component and wires it into the same wasm-side
   SQLite. The shipped 110-extension catalog and 73 dot commands
   in this repo are this configuration. Sandboxed by default,
   capability-gated, portable across OSes.

   Sub-options:
     - **AOT compilation.** `wasmtime compile cli.component.wasm`
       produces a `.cwasm` that loads in milliseconds instead of
       paying JIT cost on every invocation. The component-blob
       cache in `sqlite-cas-cache/` does this on the hot path
       automatically; standalone `.cwasm` artifacts can be
       shipped for cold-start-sensitive deployments (CLI tools,
       serverless functions, edge runtimes).
     - **Embedded extensions.** Instead of dynamic `.load`,
       extensions can be baked into the cli component at build
       time via `include_bytes!` + auto-register. The shipped
       cli already does this for core-dotcmd + all sqlite-utils-*
       (10 extensions, 73 dot commands). Same pattern works for
       any extension — produces a self-contained single-file
       distribution with zero runtime dependencies on the
       extension catalog.

3. **Browser-side SQLite-in-wasm + extensions.** Same wasm-side
   SQLite + extension components, but the host is a
   browser-resident wasm runtime instead of wasmtime. The browser
   host is built on
   [`@tegmentum/wasi-polyfill`](https://github.com/tegmentum/wasi-polyfill)
    a capability-based JS plugin framework for wasi-p1/p2/p3
   plus Web Platform host imports (DOM, fetch, WebGPU, ...). A
   jsdelivr / unpkg-shaped CDN serves extensions; the page's host
   code resolves + loads them on demand. Tracked in
   [docs/plans/PLAN-browser-runtime.md](docs/plans/PLAN-browser-runtime.md).

   Foundation scaffold lives in `browser/`: jco-transpiled
   extension components, a `sqlink.js` runtime that drives
   `sql.js` as the in-browser SQLite and registers extensions'
   scalar functions through its API, a wasi-polyfill-backed
   import map (`browser/src/wasi-imports.js`), and a Playwright
   smoke suite that exercises a curated subset of the same
   `fixtures.toml` used by scenarios 1+2. Build with
   `cd browser && npm install && npm run transpile && npm test`.

   Sub-option:
     - **Embedded extensions.** Same as scenario 2  ship a
       single bundle with extensions baked in via
       `include_bytes!`-equivalent at the JS side (a single
       webpack/rollup chunk containing the cli wasm + selected
       extensions). Zero runtime fetches; ideal for offline-
       capable PWAs, embedded docs / playgrounds, or any context
       where deferring extension load to a CDN round-trip isn't
       acceptable.

The WIT contract in `sqlite-loader-wit/` is the single point of
truth that lets the same extension binary work in all three
scenarios; the same `.component.wasm` is interchangeable between
dynamic-`.load` and static-embed modes within scenarios 2 and 3.

## What's here

```
core/                 Rust wrapper over libsqlite3-sys. The host's
                      sqlite3 connection lives here; the cli no
                      longer carries one of its own (Stage 5f
                      purged libsqlite3-sys from the cli crate).
cli/                  The cli  a wasm component that loads other
                      components via `.load`. Hot-path SQL goes
                      through the host's connection via spi;
                      dot-commands route to dotcmd-aware extensions.
host/                 The runtime. Loads + runs the cli component
                      and any extension components it `.load`s.
                      Ships as the `sqlink` binary.
sqlite-loader-wit/    The WIT contract every extension speaks.
                      Defines worlds for scalar-only, aggregate,
                      collation, vtab (read-only + mutable +
                      batched), authorizer, dot-command, and the
                      shared `spi` interface for back-channel SQL.
extensions/           ~110 extension components. Mix of ports of
                      well-known SQLite extensions (json1, fts5,
                      rtree, regexp, math, crypto, sha3, totype,
                      uint, eval, zorder, ...), general scalar
                      packs (ipaddr, ulid, isin, vin, ...), and
                      dot-command extensions (sqlite-utils-*,
                      core-dotcmd, session-cli, archive-cli, ...).
tooling/              Build + test infra. Scaffold script, smoke
                      harness, cli cheatsheet, lessons-learned doc.
provenance/           Per-extension version-tracking db.
examples/             Walk-through scripts (sqlite-utils-tour.sql
                      drives every shipped sqlite-utils command).
analysis/             Function-catalogue gap analysis vs PostgreSQL,
                      MySQL, DuckDB, ClickHouse, Snowflake, BigQuery.
```

## Quick taste

Build the host + cli once:

```bash
cargo build --release                                 # builds host (sqlink)
cargo build -p sqlite-cli --target wasm32-wasip2 \
            --release                                 # builds cli wasm
wasm-tools component new \
    target/wasm32-wasip2/release/sqlite_cli.wasm \
    -o target/wasm32-wasip2/release/sqlite_cli.component.wasm
```

Run a SQL session:

```bash
./target/release/sqlink \
    --db mydata.db \
    target/wasm32-wasip2/release/sqlite_cli.component.wasm
sqlite> CREATE TABLE t(id INTEGER, name TEXT);
sqlite> INSERT INTO t VALUES (1, 'alice');
sqlite> .tables
sqlite> .help                  -- list all available dot commands
sqlite> .help insert           -- detail for a specific command
```

The CLI is the `sqlink` binary — named after the loadable-extension
contract it ships. If you prefer the familiar `sqlite` command name,
symlink it onto your PATH:

```bash
ln -s /full/path/to/target/release/sqlink ~/.local/bin/sqlite
```

The binary doesn't look at argv\[0\], so it behaves identically under
either name.

Load an extension and use its scalars:

```bash
sqlite> .load extensions/sha3/target/wasm32-wasip2/release/sha3_extension.component.wasm
sqlite> SELECT sha3('hello', 256);
sqlite> SELECT sha3_256('hello');
```

Ingest, transform, and search à la
[sqlite-utils](https://github.com/simonw/sqlite-utils):

```bash
sqlite> .insert dogs dogs.json --pk id
sqlite> .add_column dogs adopted bool
sqlite> .transform dogs --type age real --rename age age_years
sqlite> .enable_fts dogs name breed --create-triggers
sqlite> .search dogs labrador
sqlite> .analyze_tables dogs
```

Build a new extension end-to-end:

```bash
python3 tooling/scaffold.py myext \
    --description "my new extension" \
    --world minimal
# edit extensions/myext/src/lib.rs to add scalars
make ext-ship NAME=myext   # build + smoke + regression-test the catalog
```

## Dot-command extensions

Dot commands aren't baked into the cli — they live in
`dotcmd-aware` wasm extensions that the cli auto-embeds (or that
the user `.load`s explicitly). 73 commands ship across 10
extensions today; `.help` enumerates them all.

| Extension | Commands |
|---|---|
| `core-dotcmd` | `.tables`, `.schema`, `.indexes`, `.dbinfo`, `.dbconfig`, `.fullschema`, `.lint`, `.changes`, `.timer`, `.parameter`, `.width`, `.timeout`, `.show`, `.print`, `.echo`, `.bail`, `.headers`, `.mode`, `.databases`, `.limit`, `.help`, ... |
| `sqlite-utils-schema` | `.create_table`, `.create_index`, `.create_view`, `.drop_table`, `.drop_view`, `.rename_table`, `.duplicate`, `.add_column`, `.transform`, `.extract`, `.add_fk`, `.add_fks`, `.index_fks`, `.views`, `.triggers` |
| `sqlite-utils-data` | `.insert`, `.upsert`, `.bulk`, `.insert_files`, `.rows`, `.analyze_tables`, `.convert`, `.memory` |
| `sqlite-utils-fts` | `.enable_fts`, `.disable_fts`, `.rebuild_fts`, `.populate_fts`, `.search` |
| `sqlite-utils-maint` | `.vacuum`, `.analyze`, `.optimize`, `.enable_wal`, `.disable_wal`, `.enable_counts`, `.reset_counts`, `.create_database` |
| `sqlink-meta-cli` | `.sqlink list / show / install / uninstall / bundle / unbundle / verify / gc / export` + resolver subcommands |
| `archive-cli` | `.archive` |
| `serialize-cli` | `.serialize`, `.deserialize` |
| `session-cli` | `.session create / attach / changeset / patchset / list / delete / ...` |
| `sha3sum-cli` | `.sha3sum` |

Every command is discoverable: `.help` lists them, `.help <cmd>`
renders usage, prose help, and worked examples drawn from the
extension's own manifest. Authoring guide:
[AUTHORING-DOTCMD-COMPONENTS.md](AUTHORING-DOTCMD-COMPONENTS.md).

## Extension catalog highlights

| Category | Examples |
|---|---|
| Real SQLite ext/misc/ ports | json1, regexp, math, crypto, uuid, fts5 (bundled), rtree (bundled), csv, stats, sha3 (shathree), totype, uint, eval, zorder, completion |
| Third-party SQLite extensions | extension-functions.c (Liam Healy), spellfix1, decimal, ieee754, closure, fileio, ipaddr, generate_series (vec0 family for ANN search) |
| PostGIS bridge | ~420 functions wrapping postgis-wasm / sfcgal-wasm / geos-wasm |
| Identifier validators | vin, isin, cusip, aba, bic, ean, creditcard, postcode, ssn, mac, iban, container |
| Reference data | currency (ISO 4217), country (ISO 3166), iban (ISO 13616) |
| Utility scalar packs | color, unitconv, humansize, latlon, numfmt, radix, natsort, setops, geo-distance |
| Dot-command extensions | sqlite-utils-{schema,data,fts,maint}, core-dotcmd, session-cli, archive-cli, sqlink-meta-cli (see above) |

Total: ~110 wasm component extensions.

## Where to go next

| Doc | Purpose |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | Bird's-eye view: which piece does what, why |
| [AUTHORING-DOTCMD-COMPONENTS.md](AUTHORING-DOTCMD-COMPONENTS.md) | Step-by-step: build a new `.command` |
| [AUTHORING-RUN-COMPONENTS.md](AUTHORING-RUN-COMPONENTS.md) | Step-by-step: build a new runnable component |
| `tooling/cli-cheatsheet.md` | Every dot-command + when it's useful in tests |
| `tooling/extension-patterns.md` | Shape catalog for scalar/agg/vtab/etc. |
| `tooling/lessons-learned.md` | Per-ship retrospectives |
| [PLAN-sqlite-plugins.md](docs/plans/PLAN-sqlite-plugins.md) | Catalog of shipped extensions |
| [PLAN-gaps.md](docs/plans/PLAN-gaps.md) | What's NOT shipped + ranked next-up |
| [PLAN-sqlite-utils-port.md](docs/plans/PLAN-sqlite-utils-port.md) | Stage-by-stage: porting sqlite-utils as dot commands |
| [PLAN-cli-stages-5-6.md](docs/plans/PLAN-cli-stages-5-6.md) | The CLI_CONN purge + `.session` port |
| [analysis/README.md](analysis/README.md) | Function-catalog gap analysis (6 DBs) |

## Status

Active project. Working surface (cli + host + ~110 extensions +
73 dot commands + tooling) is shippable today.

Recent milestones:
  - **CLI_CONN purge** (Stage 5f): the cli no longer carries its own
    libsqlite3-sys connection. All function/agg/coll/vtab/hook
    registration routes through the host's shared spi connection.
    Cli wasm component dropped from 2.4 MB → 1.3 MB.
  - **`.session` port** (Stage 6): changesets / patchsets ride the
    same shared connection via a new `sqlite:extension/session`
    WIT interface.
  - **sqlite-utils port**: 35 dot commands across 4 new extensions
    (schema / data / fts / maint), driven by
    [PLAN-sqlite-utils-port.md](docs/plans/PLAN-sqlite-utils-port.md).
  - **`.help` discoverability**: enumerates every registered dot
    command across every loaded extension, surfaces per-command
    usage + worked examples.

Known gaps documented in [PLAN-gaps.md](docs/plans/PLAN-gaps.md); the big
ones are hot-reload of extensions, stack-trace propagation from
wasm panics, and SpatiaLite-grade geospatial.

## License

SQLite itself is in the public domain. The wrapping code in this
repo (host, cli, extensions, tooling) is MIT.
