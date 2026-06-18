# sqlite-wasm

SQLite + an extension ecosystem, distributed as
[WebAssembly Components](https://component-model.bytecodealliance.org/).
The full SQLite C library compiles to WASI Preview 2 alongside a
cli, a host runtime, ~110 extension components, and a contract
(`sqlite-loader-wit`) that says how any wasm component becomes
an extension.

The point: you can write a SQLite extension in any language that
targets wasm (Rust, C, AssemblyScript, ...), publish it as a
`.wasm` file, and load it into the cli with `.load FILE`  the
same shape a C-side `sqlite3_load_extension` call has, but
sandboxed and portable.

## What's here

```
core/                 Rust wrapper over libsqlite3-sys; the sqlite3 the
                      cli and host both use.
cli/                  The cli  a wasm component that loads other
                      components via `.load`. Implements ~57 sqlite3-
                      cli-compatible dot-commands (.tables, .schema,
                      .dump, .backup, .session, ...).
host/                 The runtime that loads + runs the cli component
                      (and any extension components it `.load`s).
                      Ships as `sqlite-wasm-run` binary.
sqlite-loader-wit/    The WIT contract every extension speaks. Defines
                      worlds for scalar-only, vtab, collation, stateful
                      aggregate, and authorizer extensions.
extensions/           ~110 extension components. Mix of ports of
                      well-known SQLite extensions (json1, fts5, rtree,
                      regexp, math, crypto, sha3, totype, uint, eval,
                      zorder, completion, ...) plus general scalar
                      packs (ipaddr, ulid, isin, vin, currency, ...).
tooling/              Build + test infrastructure for new extensions.
                      Scaffold, smoke harness, lessons-learned doc,
                      cli cheatsheet, extension shape catalog.
provenance/           Per-extension version-tracking db.
```

## Quick taste

Build the host + cli once:

```bash
cargo build --release -p sqlite-wasm-host
cd cli && cargo build --release --target wasm32-wasip2 && cd ..
wasm-tools component new \
    cli/target/wasm32-wasip2/release/sqlite_cli.wasm \
    --adapt wasi_snapshot_preview1=$HOME/.cache/xtran/wasi_snapshot_preview1.reactor.wasm \
    -o target/wasm32-wasip2/release/sqlite_cli.component.wasm
```

Run a SQL session:

```bash
./target/release/sqlite-wasm-run \
    target/wasm32-wasip2/release/sqlite_cli.component.wasm \
    --db mydata.db
sqlite> CREATE TABLE t(id INTEGER, name TEXT);
sqlite> INSERT INTO t VALUES (1, 'alice');
sqlite> .tables
sqlite> .dump
```

Load an extension and use its scalars:

```bash
sqlite> .load extensions/sha3/target/wasm32-wasip2/release/sha3_extension.component.wasm
sqlite> SELECT sha3('hello', 256);
sqlite> SELECT sha3_256('hello');
```

Build a new extension end-to-end:

```bash
python3 tooling/scaffold.py myext \
    --description "my new extension" \
    --world minimal
# edit extensions/myext/src/lib.rs to add scalars
make ext-ship NAME=myext   # build + smoke + regression-test the catalog
```

## Where to go next

Documentation, by depth:

| Doc | Purpose |
|---|---|
| `tooling/cli-cheatsheet.md` | Every dot-command + when it's useful in tests |
| `tooling/extension-patterns.md` | Shape catalog for new extensions (classifier, validator, parser-union, vtab, collation, ...) |
| `tooling/snippets/README.md` | Paste-and-own code helpers shared across extensions |
| `tooling/lessons-learned.md` | Per-ship retrospectives; "why we made this choice" archive |
| `.claude/commands/new-extension.md` | Step-by-step workflow for shipping a new extension |
| `PLAN-sqlite-plugins.md` | Catalog of what's shipped, by source (ports vs general packs) |
| `PLAN-gaps.md` | What's NOT shipped, what's next |
| `PLAN-interactive-capture.md` | Architectural plan for the session capture half (deferred) |
| `PLAN-tooling-and-session.md` | Tooling + session deferred work |

## Extension catalog highlights

| Category | Examples |
|---|---|
| Real SQLite ext/misc/ ports | json1, regexp, math, crypto, uuid, fts5 (bundled), rtree (bundled), csv, stats, sha3 (shathree), totype, uint, eval, zorder, completion |
| Third-party SQLite extensions | extension-functions.c (Liam Healy), spellfix1, decimal, ieee754, closure, fileio, ipaddr, generate_series (vec0 family for ANN search) |
| PostGIS bridge | ~420 functions wrapping postgis-wasm / sfcgal-wasm / geos-wasm |
| Identifier validators | vin, isin, cusip, aba, bic, ean, creditcard, postcode, ssn, mac, iban, container |
| Reference data | currency (ISO 4217), country (ISO 3166), iban (ISO 13616) |
| Utility scalar packs | color, unitconv, humansize, latlon, numfmt, radix, natsort, setops, geo-distance |

Total: ~110 wasm component extensions.

## Status

This is an active project. Working surface (cli + host + most
extensions + tooling) is shippable today. Known gaps documented
in `PLAN-gaps.md`; the big ones are WAL mode (wasivfs missing
shared-memory file support), hot-reload of extensions, and
stack-trace propagation from wasm panics.

## License

SQLite itself is in the public domain. The wrapping code in this
repo (host, cli, extensions, tooling) is MIT.
