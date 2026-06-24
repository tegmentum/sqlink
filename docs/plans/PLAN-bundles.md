# Plan: bundles  named extension sets with optional baked binaries

## Status (2026-06-24)

Design conversation captured. Not yet started. Substrate prereqs
identified; one new host capability (`spi.spawn-build`) required.

## Motivation

Two recurring pain points the existing tooling doesn't quite cover:

1. **Repeated rebuilds of the same embedded set.** Today you can
   `sqlink compose --embed uuid,json1,case` to bake those three
   extensions into a fresh sqlink binary at build time. Running it
   again with the same extension list still spawns a full build 
   the cas-cache deduplicates extension BYTES but doesn't
   remember "you already built sqlink with this exact set."

2. **No reproducible round-trip from a live connection.** A common
   workflow: open a db, dynamically `.load` some extensions to
   explore, then want to capture that configuration so collaborators
   can launch sqlink with the same extension set. Today that means
   transcribing the `.load` lines and rebuilding by hand.

Bundles solve both:

- A **bundle** is a named, content-addressed set of (extension-name,
  content-hash) tuples. Identity is the sorted hash of its members.
- Each bundle MAY have one or more baked binaries (one per target
  triple). The binary is cached by the bundle's set-hash; same
  members  same hash  cache hit  no rebuild.
- `.bundle save NAME` introspects the current connection's loaded
  extensions and records the bundle. By default also builds the
  current-target binary.
- `sqlink --bundle NAME db.sqlite` launches with that bundle 
  exec's the baked binary if it exists, otherwise starts regular
  sqlink and auto-loads each extension from cas-cache.

## Naming

The name is `bundle`, not `snapshot`. SQLite's `sqlite3_snapshot_*`
is a real API for WAL read snapshots; "snapshot" too strongly
implies database state (schema + data). A bundle is a saved set of
LOADED EXTENSIONS  the metaphor is webpack/jar/app bundles, not
database snapshots. "Compose" was the runner-up since `sqlink
compose --embed` is the existing CLI verb for the same operation,
but it overloads with wasm component-model composition.

## Architecture

### Storage (extending sqlite-cas-cache)

The existing `sqlite-cas-cache` already content-addresses extension
bytes. Bundles extend the same database with three new tables:

```sql
CREATE TABLE bundles (
  id INTEGER PRIMARY KEY,
  name TEXT UNIQUE,            -- user-given handle; nullable
  set_hash TEXT NOT NULL,      -- hashed sorted (ext_name, content_hash) tuples
  created_at INTEGER,          -- epoch seconds
  last_used_at INTEGER         -- updated on every --bundle launch / show / build
);

CREATE TABLE bundle_members (
  bundle_id INTEGER REFERENCES bundles(id) ON DELETE CASCADE,
  extension_name TEXT,         -- e.g. "uuid"
  content_hash TEXT REFERENCES cas_blobs(hash),
  PRIMARY KEY (bundle_id, extension_name)
);

CREATE TABLE bundle_binaries (
  bundle_id INTEGER REFERENCES bundles(id) ON DELETE CASCADE,
  target_triple TEXT,          -- e.g. "aarch64-apple-darwin"
  binary_path TEXT,            -- absolute path to cached executable
  built_at INTEGER,
  PRIMARY KEY (bundle_id, target_triple)
);
```

A bundle is uniquely identified by `set_hash`; the `name` is an
optional human label. Looking up by name is a join; looking up by
hash-prefix is a `LIKE` scan (bounded since the cas-cache is
small). Identical extension sets reuse the same `bundle` row 
multiple `name`s pointing at the same `set_hash` is allowed (alias
semantics).

### Substrate addition: `spi.spawn-build` capability

A bundle's baked-binary build needs to spawn `cargo`. WASM
extensions are sandboxed; they can't shell out. The natural fit is
a new host-resident SPI capability mirroring the dispatch-bridge
pattern from #429/#432/#433/#436/#439/#440:

```wit
// sqlite-loader-wit/wit/host-spi.wit
interface build {
  use types.{sqlite-error};

  /// Spawn a cargo build against a generated crate. The host
  /// validates the crate root path against the workdir grant,
  /// spawns cargo with --target if provided, captures stdout/
  /// stderr, returns the produced binary path on success.
  spawn-build: func(
    crate-root: string,
    target-triple: option<string>,
    env: list<tuple<string, string>>,
  ) -> result<build-out, sqlite-error>;
}

record build-out {
  binary-path: string,
  stdout: string,
  stderr: string,
}
```

New `Capability::SpawnBuild` gates it. Same shape as
`Capability::S3` / `Capability::WalFrames`. Substrate effort: ~1
day mirroring #440's pattern.

### `bundle-cli` extension

A new dot-cmd extension joining the cli-family (`serialize-cli`,
`session-cli`, `archive-cli`, `sqlite-utils-*`, etc.). Auto-bundled
in the default cli embed list.

It declares `Capability::Spi`, `Capability::SpawnBuild` (for
`.bundle save` / `.bundle build`), and uses wasi:filesystem for
the crate emission step.

The bundle metadata tables live in the cas-cache; the extension
reads/writes via `spi.execute` against the cas-cache connection.
(Sqlink already has a SPI surface for this  the cas-cache is
host-managed; check whether a sister SPI is needed or if the
existing one suffices.)

### Build mechanism

In-process:
1. `bundle-cli`'s `.bundle save NAME` (or `build NAME`) reads the
   connection's loaded extensions via `spi-loader.list-extensions`.
2. Hashes the sorted set; queries `bundles` for a hit.
3. On miss: writes a small generated crate to a tempdir via
   wasi:filesystem. Crate contains:
   - `[bin] name = "sqlink"` with `path = "src/main.rs"`
   - `src/main.rs` reuses sqlink-host's main but with
     `include_bytes!()` baking each extension's bytes from
     cas-cache.
   - `Cargo.toml` with workspace path-deps to sqlink-host +
     sqlink-loader-wit etc. via absolute paths sourced from the
     host's installation prefix.
4. Calls `build.spawn-build(crate_root, target_triple, env)`.
5. On success: records the binary path in `bundle_binaries` +
   returns to the user.

The generated-crate template lives in the extension. It's small
(~30 lines of boilerplate). The "where is sqlink-host's source"
question  the host install needs to expose its source path via
an env var or a known location. For dev, the workspace root works.
For installed binaries, ship the source tree (or a precompiled
.a/.rlib + a much smaller crate that just links it).

For v1: dev-workspace-only is fine. Production install ergonomics
is a v2 concern.

## Surface

### Dot commands (in `bundle-cli`)

```sql
-- create a bundle from currently-loaded extensions
.bundle save myset                  -- creates + builds binary for current target
.bundle save --no-build myset       -- creates without building
.bundle save --name myset           -- equivalent to above

-- build a binary for an existing bundle
.bundle build myset                          -- current target
.bundle build myset --target wasm32-wasip2   -- specific target

-- inspect
.bundle list                        -- table: name | hash | extensions | first built | last used
.bundle show myset                  -- members + binary metadata
.bundle show 4c8e1a                 -- by hash-prefix

-- prune
.bundle delete myset
.bundle gc --keep 10                -- LRU evict
.bundle gc --older-than 30d
```

### Launch flag (in sqlink-host's argparse)

```bash
# auto: exec baked binary if exists for current target, else dynamic-load
sqlink --bundle myset db.sqlite

# force baked binary; error if not built
sqlink --bundle-baked myset db.sqlite

# force regular sqlink + auto-load; skip binary even if cached
sqlink --bundle-load myset db.sqlite

# hash-prefix lookup
sqlink --bundle 4c8e1a db.sqlite
```

Resolution order for `--bundle NAME`:
1. Look up by `name = ?` (exact). If found:
   a. If a `bundle_binaries` row exists for the current target: exec
      that binary. argv is rewritten (`--bundle` flag stripped,
      remaining args passed through).
   b. Else: start regular sqlink. Before user input, auto-`.load`
      each member from cas-cache.
2. Else look up by `set_hash LIKE 'NAME%'` (prefix). Same
   exec-or-load logic.
3. Else error: "bundle 'NAME' not found in cas-cache".

### Capability requirements

| Surface | Capability needed |
|---|---|
| `.bundle save NAME` | Spi (read loaded exts) + SpawnBuild (build binary) + wasi:filesystem grant for tempdir |
| `.bundle save --no-build NAME` | Spi only |
| `.bundle build NAME` | SpawnBuild + wasi:filesystem |
| `.bundle list` / `show` / `delete` / `gc` | Spi (cas-cache read/write) |

## v1 scope

- Extensions only (no schema, no data, no settings). Bundle =
  named set of extension content-hashes + optional baked binary.
- In-process build. Sqlink dev-workspace path required for the
  generated crate's deps.
- Single-target builds. `--target X` works but each target
  requires a separate `.bundle build`.
- LRU + age-based gc.

## Out of scope (v2+)

- **Bundle includes schema** (`.bundle save --with-schema`): also
  capture the connection's `sqlite_master` DDL so the bundle can
  reconstruct an empty db with the right tables. Probably 1-2
  days.
- **Bundle includes seed data** (`.bundle save --with-data`):
  overlaps with `wal-archive` and `litestream`-shaped territory.
  Don't do.
- **Production-install build mechanism**: ship sqlink-host as a
  rlib so the generated crate is tiny and self-contained, not
  dependent on the dev workspace. Probably 2-3 days of build
  infra.
- **Cross-target build**: invoke wasi-sdk/zig-cc/cross to build
  for targets the local toolchain can't produce natively. Per
  user, not per-bundle.
- **Bundle publishing**: push bundle metadata to a registry so
  others can `sqlink --bundle org/myset` and the cas-cache fetches
  the members. Separate plan.
- **Snapshot-the-database**: explicitly NOT in scope; that's
  `wal-archive` / `serialize-db` territory.

## Effort estimate

| Piece | Effort |
|---|---|
| **Substrate**: `spi.spawn-build` capability + `Capability::SpawnBuild` (mirror #440 pattern) | 1 day |
| `bundle-cli` extension scaffold + WIT + manifest declares | 0.5 day |
| Cas-cache schema additions + migration | 0.5 day |
| `.bundle save` (record-only path) | 0.5 day |
| `.bundle save` (with build): generated-crate template + spawn-build wire-up | 1.5 days |
| `.bundle list` / `show` / `delete` / `gc` | 1 day |
| `sqlink --bundle NAME` flag + resolution logic + exec OR auto-load | 1 day |
| Native integration tests (covers bundle round-trip + cache hit + launch flag) | 1.5 days |
| Browser smoke / docs | 1 day |

**Total: ~8 days for v1.**

## Dependencies

- `spi.spawn-build` substrate (this plan, ~1 day)
- `spi-loader.list-extensions` (already exists from #427/#433)
- cas-cache schema migrations (already supported)
- Working sqlink dev workspace for generated-crate compilation (already
  what every developer has)

## Sequencing

1. Land **`spi.spawn-build` + `Capability::SpawnBuild`** as a
   small precursor (~1 day). Filed separately so the substrate
   change is reviewable on its own.
2. Build `bundle-cli` end-to-end (per the stages above).
3. Wire `--bundle` flag in sqlink-host.
4. Native integration tests + browser smoke + docs.

## Resolved design decisions (gap pass, 2026-06-24)

Three substrate gaps surfaced when #446 started scope investigation
and the plan's stated APIs didn't match the actual codebase:

- **Gap A: enumerating loaded extensions.** The plan referenced
  `spi-loader.list-extensions` from #427/#433. That doesn't exist;
  `list-extensions` lives in the CLI-facing `sqlite:wasm/extension-
  loader` world which extensions can't import. **Decision: add
  `list-loaded-extensions` to `loader-bridge.wit`**, mirroring how
  `extension-digest` already works. Smallest WIT delta; reuses the
  bridge capability bundle-cli already needs.

- **Gap B: cas-cache reachability for bundle metadata.** The plan
  said bundle-cli reads/writes the cas-cache via `spi.execute`.
  `spi.execute` runs against the user's main connection, not the
  host-managed cas-cache. **Decision: add a new `bundles` SPI
  interface to `host-spi.wit`** with ~6-8 methods (`save`, `list`,
  `show`, `delete`, `gc`, `find`, `record-binary`) and gate it
  with new `Capability::Bundles`. Host serves CRUD against the
  cas-cache; bundle-cli does UX + planning. Mirrors the dispatch-
  bridge pattern from #429/#432/#433/#436/#439/#440.

- **Gap C: spawn-build capability for default cli's bundle-cli
  load.** Since bundle-cli ships in the default cli embed list,
  every cli session loads it. **Decision: default cli grants
  Spi + Bundles + (read-only) filesystem only.** `.bundle save NAME`
  without `--no-build` errors helpfully: `"spawn-build capability
  not granted. Re-run with sqlink --grant spawn-build, or use
  .bundle save NAME --no-build to record metadata only."` Security-
  by-default; the error message names the fix.

- **Path-prefix correction**: the plan said
  `~/.cache/xtran/builds/<hash>/`. The actual cas-cache is at
  `~/.cache/sqlink/cas.sqlite` (per `SqliteCasStore::
  default_external_path`). **Build dir is
  `~/.cache/sqlink/builds/<hash>/`** to match.

- **Gap E (mid-implementation, 2026-06-24): manifest model
  blocks the Gap C UX.** `policy.check_manifest` enforces
  *declared ⊆ granted* strictly; bundle-cli must declare
  `SpawnBuild` to import the `build` interface, but declaring it
  without the grant fails the load. So Gap C's intent ("default
  cli loads bundle-cli with Bundles only; `.bundle build` errors
  helpfully at call time") is unreachable under the current
  model. **Decision: add `optional-capabilities` to the manifest
  WIT type.** Small additive change:
    - Manifest gains `optional-capabilities: list<capability>`
      alongside the existing `capabilities` (now interpreted as
      the *required* set).
    - `check_manifest` enforces `required ⊆ granted` only;
      optional caps can be declared without being granted.
    - Bundle-cli declares `Bundles` as required, `SpawnBuild`
      as optional.
    - Runtime call to `spi.spawn-build` still fails closed via
      the existing `spawn_build_granted` flag; bundle-cli
      translates SQLITE_PERM into the Gap C error message.
  Existing extensions unchanged (their `capabilities` IS the
  required set; default `optional-capabilities = []`).
  Generalizes for any future "may use X if granted" pattern.

- **Gap D (mid-implementation, 2026-06-24): build path needs
  package + features, not a generated crate.** The plan implied
  `.bundle build NAME` would generate a tiny crate that
  `include_bytes!()`s each extension and depends on sqlink-host
  as a library. `spawn-build`'s current 1-stage `cargo build
  --release <crate-root>` contract can't drive `sqlink compose
  --embed X,Y,Z` (cargo + wasm-tools, 2 stages). **Decision:
  extend `spi.spawn-build` WIT signature** with
  `package: option<string>` + `features: list<string>` so
  bundle-cli calls `cargo build -p sqlite-cli
  --features embed-uuid,embed-json1,...` directly against the
  workspace. Additive WIT change; per-world re-widening; host
  impl gains feature-flag passthrough. Bundle-cli's `.bundle
  build` drops the v1.1-deferred stub and wires the new params.
  Removes the need for the generated-crate template entirely
  (sqlink-host as rlib still v2 production-install concern).

## Resolved design decisions (open-question pass)

1. **Build dir location**: **cas-cache-managed.** Always materialize
   the generated crate under `~/.cache/xtran/builds/<bundle-hash>/`.
   Tied to the cas-cache gc lifecycle. Re-uses the same dir on
   subsequent builds of the same bundle so incremental cargo cache
   works for free. No env-var override in v1; users who need
   explicit control will hit the question later.
2. **Generated-crate dep resolution**: **`SQLINK_DEV_ROOT` env var
   with compile-time fallback.** The bundle-cli extension's
   generated `Cargo.toml` resolves `sqlink-host` / `sqlink-loader-wit`
   via `$SQLINK_DEV_ROOT` if set; otherwise it uses the workspace
   path baked at bundle-cli build time. Works out-of-box in dev
   (sqlink built in workspace  compile-time path is valid).
   Installed binaries on a clean machine fail with a clear "set
   SQLINK_DEV_ROOT to your sqlink source checkout" error. The
   "ship sqlink-host as an rlib" production-install path stays
   in v2.
3. **Auto-load behavior on cache miss**: **error with helpful
   message.** When `sqlink --bundle myset` falls back to auto-load
   and a member's content-hash isn't in cas-cache, exit non-zero
   with: "bundle myset references extension <name> (sha=<hash>)
   which isn't in cas-cache. Run .load /path/to/<name>.component.wasm
   to refill, or rebuild the baked binary with .bundle build myset."
   No auto-magic. v2 registry-fetch slots in cleanly later (would
   become a fallback before the error).

## References

- `sqlite-cas-cache/` (the existing cas-cache; extending its
  schema is this plan's main storage move)
- README's `sqlink compose --embed NAME[,NAME...]` (existing CLI
  verb that's roughly the imperative version of what `.bundle save
  --no-cache` does)
- Dispatch-bridge pattern from #429/#432/#433/#436/#439/#440 (the
  capability-shaped SPI extension model that `spi.spawn-build` will
  mirror)
