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

     - **`sqlink-loader` SQLite loadable extension**. Same
       dispatch surface as `sqlink-native`, but packaged as a
       cdylib that vanilla `sqlite3` can side-load. Working for
       scalars and aggregates as of option B's first cut; see
       [`sqlink-loader/DESIGN.md`](sqlink-loader/DESIGN.md) for
       the full surface inventory.

       ```bash
       cargo build --release -p sqlink-loader
       # Resolve extension names against the workspace target.
       export SQLINK_LOADER_REPO_ROOT=$PWD
       # Eager-load via env, no per-extension SQL calls needed.
       export SQLINK_LOADER_EXTS=uuid
       sqlite3 :memory: \
           "SELECT load_extension('./target/release/libsqlink_loader');" \
           "SELECT uuid();"
       # Or load at runtime once the .so is in:
       sqlite3 :memory: \
           "SELECT load_extension('./target/release/libsqlink_loader');" \
           "SELECT sqlink_load_ext('uuid', './extensions/uuid/target/wasm32-wasip2/release/uuid_extension.component.wasm');" \
           "SELECT uuid();"
       ```

       The `libsqlite3-sys` feature conflict between `bundled`
       and `loadable_extension` is sidestepped by hand-rolling the
       `sqlite3_api_routines` indirection in `src/api.rs`  the .so
       reaches the host process's sqlite3 via pApi without
       unifying any feature flag across the workspace.

       SPI back-channel: extensions calling `spi.execute(...)`
       route through a secondary in-.so SQLite connection. Set
       `SQLINK_LOADER_DB_PATH=<file.db>` to make it point at the
       same file the user opened (WAL mode recommended). With
       `:memory:` the two SQLites are necessarily distinct;
       SPI-using extensions can still load but will operate on an
       empty schema. Vtab modules / collations / hooks are tracked
       as follow-up work  the v1 cut covers scalars + aggregates,
       which is the majority of the catalog.

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

   The browser runtime lives in `browser/`: the composed
   `cli + sqlite-lib + single-memory` component (one wasm, real
   SQLite) is fetched at runtime, transpiled by jco's browser build
   inside `@tegmentum/wasi-polyfill`'s `createRuntimeBindgen`,
   and driven via JSPI (`WebAssembly.Suspending`/`promising`;
   Chrome 137+ / Node 22+). The cli's REPL stays alive across
   `db.exec()` calls  a long-lived `QueueInputStream` feeds stdin
   and a sentinel `SELECT` frames each call's stdout window. DDL,
   INSERTs, and host-registered scalars/aggregates/collations all
   persist across calls. Build with `cd browser && npm install &&
   npm run transpile && npm test`. **Phase C is fully landed**:
   composed runtime + persistent session + dispatch-bridge wiring
   for scalars, aggregates, collations, authorizers + update /
   commit / rollback / wal hooks, AND virtual-table modules;
   sql.js is gone. Every SPI registration surface a loaded
   extension can use is wired end-to-end on the composed-browser
   path  identical to scenarios 1+2 within those categories.
   See `browser/src/sqlink-composed.js`, the cross-spec test set
   (composed / composed-uuid / composed-aggregate / composed-
   collation / composed-persistent / composed-runtime-ext /
   composed-vtab / demo / embed / smoke), and
   [docs/plans/PLAN-browser-runtime.md](docs/plans/PLAN-browser-runtime.md).

   Sub-options:
     - **Embedded extensions.** Same as scenario 2  ship a
       single bundle with extensions baked in via
       `include_bytes!`-equivalent at the JS side (a single
       webpack/rollup chunk containing the cli wasm + selected
       extensions). Zero runtime fetches; ideal for offline-
       capable PWAs, embedded docs / playgrounds, or any context
       where deferring extension load to a CDN round-trip isn't
       acceptable.
     - **Dynamic load from raw bytes.** `db.loadExtension(name,
       bytes)` accepts a `Uint8Array` of an extension's
       `.component.wasm` and instantiates it in-browser via
       `createRuntimeBindgen` no `npm run transpile` step
       required. The build-time `transpile-extensions.mjs`
       remains useful for AOT-bundled / code-split extensions,
       but is no longer mandatory: a page can fetch an
       extension's component bytes from a CDN, an uploaded file,
       or `import.meta.url`-relative asset and pass them
       straight in.

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
| `bundle-cli` | `.bundle save / list / show / delete / gc / build` |

### Bundles  named extension sets

A **bundle** is a named, content-addressed set of `(extension-name,
content-hash)` tuples backed by the cas-cache. The typical
workflow: open a db, dynamically `.load` some extensions to
explore, then capture that configuration so a future session can
reload it in one command.

```
sqlite> .load uuid.component.wasm
sqlite> .load json1.component.wasm
sqlite> .bundle save myset --no-build
bundle 'myset' saved (id=1, set_hash=110535dcc1139a7c, members=2)
  9e4abf277d3ddc52  uuid
  d606de418b9d7b8d  json1

sqlite> .bundle list
NAME                 SET-HASH         MEMBERS  BINARIES  LAST-USED
myset                110535dcc1139a7c       2         0  1782330713

sqlite> .bundle show myset
bundle myset (id=1)
  set_hash:   110535dcc1139a7cb882cdaf5eaaf9de7a2b8f550aa22ad228c137a08954aa15
  ...
```

Re-launching with the bundle preloads its members from cas-cache
before the cli prompt appears:

```
$ sqlink --bundle-load myset cli.component.wasm --db file.sqlite
[bundle] 'myset': dynamic-loaded uuid (9e4abf277d3d)
[bundle] 'myset': dynamic-loaded json1 (d606de418b9d)
sqlite>
```

Launch flags:

| Flag | Behavior |
|---|---|
| `--bundle NAME` | Auto: exec baked binary for current target if present, else fall back to dynamic-load. |
| `--bundle-baked NAME` | Force baked path; error if no binary for current target. |
| `--bundle-load NAME` | Force dynamic-load; skip any baked binary. |

`NAME` accepts either an exact bundle name or a `set_hash` prefix
(ambiguous prefixes error rather than guess).

v1 ships the metadata-only path  `.bundle save / list / show /
delete / gc` and `--bundle-load`. The build path (`.bundle build`,
`--bundle-baked` exec, true name-aliasing) is v1.1: it needs a
design call on how to drive `sqlink compose` from inside a wasm
extension, plus a `__cas_bundle_alias` table for multi-name
support. The substrate (`spi.spawn-build` from #445) is wired
and ready when v1.1 lands.



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

## Testing infrastructure

The workspace ships three layers of test tooling beyond
`cargo test`:

**Fuzz testing** (cargo-fuzz, libfuzzer-based). Targets live
under `fuzz/fuzz_targets/`; the `fuzz/` crate is intentionally
outside the workspace so its nightly toolchain pin doesn't
leak. Smoke runs on every PR via `.github/workflows/fuzz-smoke.yml`
(5-minute budget per target).

```bash
# one-shot local run, 60s budget
cd fuzz && cargo +nightly fuzz run policy_check_manifest -- -max_total_time=60

# all targets, names listed by cargo-fuzz
cd fuzz && cargo +nightly fuzz list
```

Current targets: `policy_check_manifest` (capability gate),
`cas_put_bytes_roundtrip` (cas-cache content-addressing),
`bundle_save_set_hash` (bundle CRUD + alias conflict),
`parse_duration` (`.bundle gc --older-than`), `parse_load_args`
(`.load` argv).

**Mutation testing** (cargo-mutants). Workspace config in
`mutants.toml`. Nightly job runs against the dense crates per
`.github/workflows/mutants-nightly.yml`; surviving mutants flag
tests that pass too easily.

```bash
# list mutants without running (fast)
cargo mutants -p sqlite-cas-cache --list

# real run (slow — every mutant runs the full test suite)
cargo mutants -p sqlite-cas-cache
```

**Stale-component guard.** `scripts/encode-extension-components.sh`
hashes each extension's WIT closure into a sidecar; on WIT drift,
the next encode triggers a per-extension rebuild before re-encoding.
See `scripts/test-encode-wit-skew.sh` for the round-trip smoke test.

**Running CI workflows locally** (`nektos/act`). Useful for
shaking out workflow bugs before push. Install:

```bash
brew install act                     # macOS
gh extension install nektos/gh-act   # cross-platform via gh CLI
```

`act` needs a docker-compatible daemon (Docker Desktop, OrbStack,
or colima). Repo defaults live in `.actrc`.

```bash
scripts/ci-local.sh --list             # show available workflows
scripts/ci-local.sh ci                 # host-side fmt + clippy + tests
scripts/ci-local.sh wasm-smoke         # wasm side + extension-smoke
scripts/ci-local.sh fuzz-smoke         # 5 cargo-fuzz targets
scripts/ci-local.sh mutants-nightly    # cargo-mutants (long; cron event)

# pass extra args through to act
scripts/ci-local.sh ci -j compose-tests --verbose
```

The wrapper handles colima / OrbStack quirks (auto-exports
`DOCKER_HOST` from `docker context`) and picks the right event name
from each workflow's `on:` clause. Caveats:

- The container runs `actions/checkout@v4` which fetches submodules;
  the private `tegmentum/*` submodules need a token. Drop a PAT into
  `.secrets` as `GITHUB_TOKEN=ghp_...` (act reads `.secrets` by
  default; the file is gitignored).
- `mutants-nightly` mirrors a `cron` schedule; act synthesizes the
  event payload. The job itself runs the full mutation suite — kill
  early once the cargo-mutants step starts unless you want the full
  60+ minutes.
- `fuzz-smoke` requires a nightly Rust install inside the container;
  the workflow installs it via `rustup install nightly`, so first
  invocation downloads ~150 MB and is slow.

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

## Acknowledgements

The `sqlite-utils-*` family of dot-command extensions
(`sqlite-utils-schema`, `sqlite-utils-data`, `sqlite-utils-fts`,
`sqlite-utils-maint`) is a wasm-component port of the dot-command
surface from **[sqlite-utils](https://github.com/simonw/sqlite-utils)**
by **[Simon Willison](https://simonwillison.net/)**. The shape,
naming, and ergonomics of `.insert`, `.transform`, `.extract`,
`.enable_fts`, `.search`, and the other 30+ commands all trace
directly back to sqlite-utils' Python CLI. Where the semantics
match the upstream tool's docs, the upstream tool got there
first  if anything reads as familiar, that's why.

`examples/sqlite-utils-tour.sql` walks the full ported surface
and is the most direct way to verify the port behaves like the
original.

The `wal-archive` extension's design (continuous WAL-frame
shipping to object storage + periodic base snapshots +
point-in-time recovery) is heavily inspired by
**[Litestream](https://litestream.io/)** by
**[Ben Johnson](https://github.com/benbjohnson)**. The
WAL-segment shipping cadence, base-snapshot model, and
restore-from-snapshot-plus-replayed-WAL semantics all come
from Litestream; sqlink's wal-archive is a separate
implementation in a different runtime model (in-process inside
a WASM component rather than a separate Go daemon) but the
storage layout and recovery story trace directly back to
Litestream. Where the semantics match the upstream tool's
docs, the upstream tool got there first.

## License

SQLite itself is in the public domain. The wrapping code in this
repo (host, cli, extensions, tooling) is MIT.
