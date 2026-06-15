# Plan: latent cleanup items

> Small tail of items flagged during the grants / component-
> cache / wasmMachine work that don't have a complaint behind
> them. Captured here so future passes don't relitigate
> whether they're known.

## Goal

Close the small known gaps without inventing new scope. Three
buckets, prioritized by cost-to-fix vs benefit:

1. **Code health** — dead code, version-coupling smells.
2. **Plumbing optimizations** — known inefficiencies with no
   measured complaint; gate each on a profile.
3. **Feature surface tail** — partially-shipped capabilities
   from earlier plans.

## Bucket 1 — code health (~30 min)

### L1a — delete unused grants/orchestration scaffolding

Four `cargo build -p sqlite-cli` warnings persist:

| Warning | Location | Action |
|---|---|---|
| `function get_by_digest is never used` | `cli/src/grants.rs` | Delete; the host's `extension-digest` sidecar replaced the need for a digest-keyed grant lookup. |
| `function digest_hex is never used` | `cli/src/grants.rs` | Delete; same reason. |
| `method name is never used` | `cli/src/orchestration.rs` (the `OrchestrationStore::name` trait method) | Either delete or use it — the only caller would be a `.compose backend` cli command that doesn't exist yet. Delete the method; revisit when the trait grows real callers. |
| `unused doc comment` | `cli/src/settings.rs:132` (above a macro invocation) | Convert `///` to `//` per rustc's hint. |

Deliverable: `cargo build -p sqlite-cli --target wasm32-wasip2 --release` builds clean (zero warnings from cli; the postgis-bridge's `gg_to_f64_inf` warning was already cleaned).

### L1b — derive wasmtime version constant from a build-time source

`host/src/component_blob_cache.rs:wasmtime_version()` returns a
hardcoded `"45.0.1"`. A `Cargo.toml` bump won't invalidate
cached `_component_cache` blobs unless someone remembers to
update this string in lockstep.

Two options:

- **(a) Read from `Cargo.lock` at build time** via a `build.rs`
  that parses the wasmtime version and writes
  `OUT_DIR/wasmtime_version.txt`, then `include_str!`'d into the
  constant. Pro: always correct. Con: build.rs added to a crate
  that doesn't have one.

- **(b) Use `wasmtime::VERSION`-style constant** if wasmtime
  exposes one. Quick `grep -r 'pub const VERSION' ~/.cargo/registry/src/.../wasmtime-45.0.1/`
  check first — if it's there, one-line change.

**Decision**: try (b) first (5 min); fall back to (a) if not
exported (~1 hr).

## Bucket 2 — plumbing optimizations (~half day each, gated on profile)

### L2a — cache the user-db Connection on Host

`Host::component_cache_row_count`, `_total_bytes`, `_purge`
each call `component_blob_cache::open_user_conn(&db_path)`,
which runs `Connection::open(...)` + `execute_batch(SCHEMA_DDL)`.
For `.cache stats components` that's 2 open()s + 2 schema-
ensures per invocation; for `.cache gc components` it's 1 +
the purge query. Not measurable from one-shot cli usage but
real if anything programmatic polls.

**Shape**: `Host.user_conn: Arc<Mutex<Option<Connection>>>`,
lazily-initialized on first access keyed by `db_path()`. Reset
when `set_db_path()` runs. Single helper
`user_conn_lazy() -> MutexGuard<Connection>` that ensures
schema once.

**Gate**: profile `.cache stats components` first. If `open_user_conn`
isn't the top frame, skip.

### L2b — drop the wasted sha256 query when blake3 hits

`host::cache::Cache::lookup_by_hash` tries blake3 then sha256
unconditionally. Both run via `SqliteCasStore::get` →
`get_by_sha256` — two SELECTs even on the (common) blake3 hit.
Easy fix: if blake3 returns `Some(_)`, return without the
fallback. Already true in current code; verify and add a test.

Probably already fine — flagged here to document the assumption
held vs the actual code. Likely a 5-min "read the code, add a
test asserting blake3-only behavior, done."

## Bucket 3 — feature surface tail (~half day to 1 day each)

### L3a — `--trust=prompt` mode

PLAN-grants-db.md spec'd three trust modes (`manifest`,
`stored`, `prompt`); I shipped only the first two. Prompt mode
needs:

- Detect TTY (`isatty(0)` on the cli's stdin).
- After describe returns `(name, digest)`: show the user the
  ext name, digest, declared capabilities (from a second
  describe field — currently we don't surface declared_caps;
  could add it to the WIT or do a second describe pass that
  returns the full manifest).
- Read y/N from stdin; on yes, persist; on no, refuse load.
- Default to `manifest` when stdin isn't a TTY (scripts don't
  hang on a prompt).

Cost: ~half day. Touches WIT (extend `describe-extension`'s
return to carry declared_caps), host impl, cli's `do_load`.

**Gate**: ship when an interactive TTY UI is actually wanted.
Headless workflows have `manifest`; hardened workflows have
`stored`. Prompt is the middle ground for ad-hoc operators.

### L3b — `describe-extension-from-uri` non-`file:` schemes

Currently handles `file:` + `blake3:` only. Other schemes
(`https:`, `oci:`, etc.) need a resolver round-trip to fetch
bytes before describe can hash + instantiate.

The load path (`load_extension_from_uri`) already does this —
fetches via the resolver chain, caches, then loads. The
describe path could call the same fetch path, then run
`describe_extension_from_bytes` against the cached bytes.

**Shape**: extract the URI-resolution step from
`load_extension_from_uri` into a `resolve_uri_to_bytes(uri)`
helper. Both load and describe call it.

Cost: ~half day. Mostly extraction + tests for the new shape.

**Gate**: when `--trust=stored` becomes a real workflow against
URI-loaded extensions. Currently `--trust=stored` only enforces
against `file:` paths.

### L3c — diagnostic on HMAC-key creation failure

`component_blob_cache::load_or_create_hmac_key` returns
`Option<Vec<u8>>` and silently returns `None` on any error
path (`HOME` unset, `create_dir_all` fails, `/dev/urandom`
read fails, `write_mode_0600` fails on existing file). The
cache then silently degrades to no-cache mode.

Add a `tracing::warn!` (one-time, gated by a `OnceLock`) on
the first failure path so a user diagnosing "why isn't the
cache working" sees a clue.

**Shape**: `OnceLock<()>` field that gets set on first warning;
each `load_or_create_hmac_key` call checks it before emitting.

Cost: ~half hour.

## Bucket prioritization

| Item | Cost | Why now / why later |
|---|---|---|
| L1a (warnings) | 30 min | Trivial. Do anytime. |
| L1b (wasmtime version) | 5 min – 1 hr | Tiny if `wasmtime::VERSION` exists; do alongside L1a. |
| L2a (cached Connection) | half day | Profile first. Skip unless `.cache stats components` calls show up hot. |
| L2b (sha256 fallback) | 5 min | Read + test. Bundle with L1a. |
| L3a (prompt mode) | half day | Wait for ask. Headless + hardened modes cover most workflows. |
| L3b (URI describe) | half day | Wait for ask. Tied to URI-loaded extensions wanting strict trust. |
| L3c (HMAC warning) | half hour | Bundle with L1 as a "small cleanups" commit. |

## Sequencing

**One commit, bundle the trivial work**: L1a + L1b + L2b + L3c.
All small, all uncontroversial, total ~1-2 hours including
verification. Clean build + visible diagnostic on HMAC failure
+ wasmtime-version derived correctly + no wasted sha256 query.

**Profile-gated**: L2a. Don't do unless a profile shows the
inefficiency.

**Ask-gated**: L3a + L3b. Ship when a workload pushes on them.

## Out of scope

- **WIT manifest convergence**: the cli sees the
  guest-described manifest; the host wraps it for the loader-
  facing return. A unified type would eliminate the
  conversion site in `manifest_for_ext` but is a moderate
  refactor with no measured impact.
- **Component cache fault recovery**: if the user db is
  corrupted, `_component_cache` lookups error and the cache
  goes silent. Already correct (degrades to no-cache); no
  recovery story needed unless a workload demands it.
- **Eviction policy tuning**: LRU on `last_used_at` is fine;
  no plan to add LFU or size-weighted variants without a
  workload that demonstrates the need.

## Notes

- The 4 cli warnings + 1 wasmtime-version constant were
  visible during the C2 / E1 work and never cleaned up
  because the plans were already getting long. This document
  is the followup.
- L2a's "open fresh Connection" pattern is the same one the
  cas-cache used pre-CP8 and was accepted there for the same
  reason (no measured complaint). If a complaint surfaces for
  either subsystem, fix both in the same pass.
