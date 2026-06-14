# Plan: Move the CAS cache into SQLite (two-tier with migration)

## Goal

Replace the filesystem-backed extension cache (`host/src/cache.rs`,
`~/.cache/sqlite-wasm/extensions/`) with a SQLite-backed
content-addressed store that supports two operating modes plus
explicit migration between them.

- **External mode** — single shared SQLite db at a well-known
  location. Multiple cli invocations / multiple processes on the
  same machine share it. Deduplicates by blake3 hash across runs.
- **Internal mode** — CAS tables embedded inside the user's
  working SQLite db. Self-contained: ship the .db file, the
  extensions go with it.

Migration commands move artifacts between the two modes
explicitly (copy semantics, not move — partial-migration
interruption can't lose data).

## Out of scope

- **wasmMachine's artifact store.** wasmMachine has its own
  content-addressed store for kernel / rootfs / tool blobs, owned
  by the wasmMachine runtime with lifecycle tied to machine
  identity. Both stores use blake3 because blake3 is the right
  primitive, but they are independent stores with independent
  eviction and independent migration. See `PLAN-wasmmachine.md`.

## Schema

Same tables in both modes. `__cas_` prefix on every table so the
internal-mode embedding into a user db doesn't collide with their
schema.

```sql
-- One row per unique content. Insert-or-ignore on blake3.
CREATE TABLE __cas_artifact (
    hash         BLOB PRIMARY KEY,    -- blake3 (32 bytes)
    bytes        BLOB NOT NULL,       -- the wasm component bytes
    bytes_len    INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,    -- unix epoch
    last_used_at INTEGER NOT NULL,    -- updated on resolve
    use_count    INTEGER NOT NULL DEFAULT 0
);

-- URI to content mapping. Mutable. ON DELETE RESTRICT so
-- artifact rows can't vanish out from under a uri_index entry.
CREATE TABLE __cas_uri (
    uri          TEXT PRIMARY KEY,
    hash         BLOB NOT NULL REFERENCES __cas_artifact(hash) ON DELETE RESTRICT,
    fetched_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);
CREATE INDEX __cas_uri_hash ON __cas_uri(hash);

-- Config + opt-in stats. Replaces the old uri_index/*.json files'
-- ad-hoc metadata.
CREATE TABLE __cas_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

## Pluggable artifact resolvers

The current host's resolver system (`register_resolver(scheme,
path, policy)` wiring wasm components per scheme) was bolted onto
the host; the SQLite CAS makes resolution a first-class
abstraction. Adopts v86's `ArtifactRef` shape  multiple sources
per artifact, tried in order until one resolves.

```rust
pub trait ArtifactResolver: Send + Sync {
    fn supported_kinds(&self) -> &[&str];
    fn resolve(&self, source: &Source) -> Result<Vec<u8>>;
}

pub struct Source {
    pub uri: String,
    pub kind: String,
}

pub struct ArtifactRef {
    pub digest: Hash,
    pub size: u64,
    pub media_type: Option<String>,
    pub sources: Vec<Source>,
}
```

`SqliteCasStore::fetch_artifact(artifact)` checks the local cache
by digest first, then walks `sources` in order  first
successful resolution wins. Returned bytes get digest-verified
before storing (mismatch = error, source advanced).

### What ships built-in

- **`local:`**  filesystem; always on, no feature flag
- **`blake3:`**  lookup by hash in the local store; always on
- **`https:`**  reqwest-backed; behind a default-on `https`
  feature flag in `sqlite-cas-cache`. Disable with
  `default-features = false` for the minimum dep surface.

Other v86 ArtifactRef transports (`s3`, `oci`, `ipfs`, `iroh`)
get added as feature flags only when concrete needs surface 
no point shipping aws-sdk in the binary by default.

### Wasm-component bridge

Existing `register_resolver(scheme, path, policy)` flow keeps
working via a separate `sqlite-cas-wasm-resolver` crate that
implements `ArtifactResolver` over the host's wasm-component
resolver mechanism. Keeps `sqlite-cas-cache` native-only (no
wasmtime dep); host wires the bridge to register wasm-based
resolvers into the store's registry. One mechanism (the trait),
two registration paths (native, wasm-component).

## Implementation shape

New host-side crate `sqlite-cas-cache` replaces `host/src/cache.rs`:

```rust
pub trait CasStore {
    fn put(&mut self, bytes: &[u8]) -> Result<Hash>;
    fn get(&self, hash: &Hash) -> Result<Option<Vec<u8>>>;
    fn set_uri(&mut self, uri: &str, hash: &Hash) -> Result<()>;
    fn resolve_uri(&self, uri: &str) -> Result<Option<(Hash, Vec<u8>)>>;
    fn list(&self) -> Result<Vec<UriEntry>>;
    fn purge(&mut self) -> Result<u64>;
    fn gc(&mut self) -> Result<u64>;
    fn evict_lru(&mut self, target_bytes: u64) -> Result<u64>;
}

pub struct SqliteCasStore {
    conn: Connection,
    mode: StoreMode,
    config: StoreConfig,
}

pub enum StoreMode {
    External(PathBuf),     // separate db file at this path
    Internal,              // shared connection owned by caller
}

pub struct StoreConfig {
    pub max_bytes: Option<u64>,    // LRU cap; None = unbounded
    pub gc_on_put: bool,           // run gc opportunistically
}
```

## Decisions locked in

| | |
|---|---|
| Default mode | **External**, auto-creates `~/.cache/sqlite-wasm/cas.sqlite` on first use. Internal mode is opt-in via `.cache use-internal`. |
| Eviction | **LRU with user-configurable size cap.** Default 1 GiB; configurable via `.cache config max-bytes <N>` or env var. `evict_lru` walks `__cas_uri ORDER BY last_used_at` dropping entries until under cap; only orphaned `__cas_artifact` rows (no uri_index reference) get purged. |
| Back-compat | **Replace `.cache *` surface entirely.** Drop the filesystem-cache dot-command behavior; reimplement against the new store. No `.cas` parallel surface. |

## Cli surface

| Command | Mode | Behavior |
|---|---|---|
| `.cache mode` | both | print active mode + path + size + LRU cap |
| `.cache use-external [PATH]` | both | switch to external mode; default `~/.cache/sqlite-wasm/cas.sqlite` |
| `.cache use-internal` | both | switch to internal mode (current db) |
| `.cache config max-bytes <N>` | both | set LRU cap; `0` = unbounded |
| `.cache config max-bytes` | both | print current LRU cap |
| `.cache list` | both | uri → hash, sizes, last_used_at |
| `.cache stats` | both | total bytes, count, hit rate |
| `.cache export PATH` | both | dump current store to a portable db file |
| `.cache import PATH` | both | load from a portable db file (idempotent on hash collision) |
| `.cache migrate-to-internal` | external | copy artifacts referenced by uri_index into current db |
| `.cache migrate-to-external [PATH]` | internal | copy this db's `__cas_*` into external store |
| `.cache purge` | both | DELETE all rows |
| `.cache gc` | both | drop artifacts with no uri_index reference |

## Migration semantics

- `migrate-to-*` copies, doesn't move. Source survives. Operator
  purges the source explicitly after verifying the copy.
- All copies use `INSERT OR IGNORE` against `__cas_artifact.hash`
  and `INSERT OR REPLACE` against `__cas_uri.uri` so duplicate
  artifacts merge cleanly.
- Each migration runs inside one transaction; partial failures
  roll back.

## Validation

- Round-trip: put → set_uri → resolve_uri returns the same bytes
- External + Internal both pass the same trait-based test suite
- LRU: fill to 110% of cap, assert cap-respecting cleanup happens
  on the next put
- Migration: external → internal → external round-trips identical
  content (`hash` columns match)
- `.cache export` / `import` between two stores preserves all
  uri_index entries

## Integration points

- `Host::set_cache` — accepts `SqliteCasStore` in either mode
- cli `.cache *` dot commands route through `Host` to the store
- `register_resolver`-driven downloads write to the active store
- The store implements `host::cache::Cache` (current trait) so
  the rest of the host plumbing doesn't change

## Open questions

None. Ready to implement when sequenced.

## Deferred (re-evaluate when CP8 lands)

- **sha256 mirror for compose `resolve_by_digest`.** The prior
  filesystem cache wrote artifacts under both `blake3/<hash>.wasm`
  and `sha256/<hash>.wasm`, and `Cache::lookup_by_hash` tried both
  prefixes. The SQLite schema indexes blake3 only — sha256 lookups
  now miss. This is acceptable today because
  `linker::Host::resolve_by_digest` (host/src/lib.rs:~395) always
  returns an error after the cache hit ("found in cache but wasm-
  component providers aren't instantiated in v1") — i.e., the dual
  lookup never served a working flow. When CP8 wires the actual
  wasm-component provider instantiation, add a `sha256 BLOB` column
  to `__cas_artifact` (+ unique index, schema bump) so the digest
  lookup works either way without changing the upper-layer call.

## Out of scope (named so they don't get assumed)

- **S3 / OCI / IPFS / iroh resolvers** in the first ship. The
  trait + registry handles them; concrete impls land when a
  user need surfaces. The trait shape is stable.
- **Distributed CAS** (multi-node, replication). Each cas.sqlite
  is single-node. wasmMachine's own store handles cross-node
  artifact distribution at a different layer.
