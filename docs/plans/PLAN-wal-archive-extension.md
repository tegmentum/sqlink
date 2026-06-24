# Plan: `wal-archive` extension  continuous WAL backup for sqlink

## Status (2026-06-24)

**v1 implementation landed on the `wal-archive-extension` branch.**
The consumer extension (`extensions/wal-archive/`) ships against
the four substrate landings (#438 native wal-hook wiring, #439
wal-frames SPI, #440 s3-base SPI, #441 cached hook+scalar Store
unification). Native end-to-end tests pass under both deployment
paths (sqlink-native + sqlink+wasm-cli) via a mock S3 server:

  * `wal_archive_start` parses opts, pulls sidecar state from S3,
    installs in-Store state.
  * The wal-hook callback drains frames via wal-frames::read-frames,
    buffers them, and flushes compressed segments to S3 when the
    byte/time threshold fires.
  * `wal_archive_snapshot_now()` serializes the db via
    spi.serialize-db, compresses, ships to S3, GCs older WAL
    segments.
  * `wal_archive_restore()` pulls latest.snap.lz4 + the sidecar,
    deserializes into the spi connection's main via
    spi.deserialize-db, and (best-effort) tries
    spi.backup-into(target_path). WAL replay past the snapshot is
    deferred to v2 (see below).

**v2 follow-ups** (none of which block v1 shipping):

  1. **WAL replay past the snapshot.** v1 surfaces a count of
     skipped segments but doesn't actually apply them. Two
     wedges block it under sqlink-native today: (a)
     wasmtime-wasi's `in_tokio` does `block_on` without
     `block_in_place`, so wasi:filesystem ops after async Host
     calls (s3-base) in the same scalar dispatch panic with
     "Cannot start a runtime from within a runtime"; (b)
     `spi::backup_into` after `spi::deserialize-db` currently
     returns SQLITE_CANTOPEN under sqlink-native + sqlink+wasm-
     cli  needs investigation. Either route through a different
     IO bridge OR a `spi::write-file` substrate addition could
     close this.
  2. **Timer-driven snapshots.** v1 ships with on-demand
     `snapshot_now()` only. The opts JSON's
     `snapshot_interval_seconds` field is parsed + stored for the
     follow-up.
  3. **zstd compression option.** v1 ships lz4-only. The opts
     JSON should grow a `compression` field once a zstd alternative
     lands.
  4. **`sqlink --backup` CLI flag** (Stage 7 stretch). v1 requires
     explicit `.load wal-archive ... + SELECT wal_archive_start(...)`.
  5. **Browser deployment.** #437 (vfs-tvm WAL support) and #444
     (browser-side s3-base / wal-frames / wal-hook / metadata /
     spi-loader stubs) have both landed. The composed runtime's
     in-wasm VFS now honors `PRAGMA journal_mode=WAL` via the
     iVersion=2 io_methods + xShm* family + per-file lock-level
     bookkeeping, and the browser extension-imports layer stubs
     out the newer `sqlite:extension/*` interfaces so hookprobe
     and wal-archive can instantiate (full browser playwright
     suite is now 12/12). What's left for a real browser
     deployment of wal-archive: the s3-base stubs return
     `SQLITE_ERROR` when called, so the wal-archive S3 backend
     needs a fetch+SigV4 browser polyfill before it can actually
     ship frames. And the sqlite-lib wal-frames stub still
     returns None — wiring it through the now-WAL-capable VFS
     is a follow-up too.

Reframe + rename of `PLAN-browser-litestream.md`. Two changes from
that earlier doc:

1. **Scope widens from browser-only to every sqlink deployment.**
   The original framed this as a browser feature because we anchored
   on a single argument: "in-process matters here, a separate daemon
   works fine elsewhere." On a second look, in-process matters
   everywhere there's no first-class process model OR where
   operational overhead matters  edge runtimes, serverless,
   mobile, embedded, multi-tenant SaaS, CLI ergonomics, distributed
   SQLite, and the browser.
2. **Renamed from `litestream` to `wal-archive`.** The extension's
   design is heavily inspired by Ben Johnson's
   [Litestream](https://litestream.io/)  same primitive (continuous
   WAL frame shipping + periodic base snapshots) and broadly the
   same restore semantics  but it's a separate implementation in a
   different runtime model, so it gets its own functional name.
   Litestream + Ben Johnson get credited in the project README's
   Acknowledgements section alongside Simon Willison + sqlite-utils.

This plan covers all deployments with **one `.wasm` extension** built
on the dispatch-bridge substrate that already serves scalars,
aggregates, and collations  one new WAL-hook surface added to
`dispatch-bridge`, plus a Rust extension that imports the existing
`~/git/s3-wasm` component for storage.

## Why a sqlink-internal wal-archive beats running upstream Litestream alongside

| Scenario | Why in-process wins |
|---|---|
| **Browser** | No process model. Cloudflare Workers / Vercel Edge / Fastly Compute / Deno Deploy same constraint. Litestream daemon literally cannot run. |
| **Serverless** (Lambda, Cloud Functions) | Short-lived process; Litestream's "shipping happens between commits" assumes a long-lived daemon. In-process can flush per-commit or at function-exit. |
| **Mobile** (iOS/Android via wasm) | No background daemon allowed by platform. |
| **Embedded / IoT / kiosks** | One fewer binary to package, configure, restart, monitor. systemd unit on a Raspberry Pi is one more failure mode. |
| **Multi-tenant SaaS** | 1000 tenants × 1 daemon each = 1000 processes. One sqlink with 1000 connections each managing its own internal backup = 1 process. |
| **CLI ergonomics** | `sqlink --backup s3://my-bucket/path/` is a flag. Upstream Litestream needs a yaml config + a separate invocation. |
| **Distributed SQLite** (Marmot/rqlite shape) | The storage sink isn't S3-specific  with the right backend it's another sqlink instance. Peer-to-peer WAL shipping without external infra. |
| **Complement to session/changeset** | Logical replication (changesets, cross-version, cross-schema) and physical replication (WAL segments, byte-identical PITR) are both useful for different things. Having both as sqlink extensions beats running Litestream + a separate changeset tool. |

## Substrate prerequisites (revised 2026-06-23 after design conversation)

Four substrate pieces land before the extension itself  the
original plan assumed all four were in place, they weren't. Each
is ~1-2 days; total ~5 days of substrate before the extension's
~12 days.

### Substrate 1 (#438): native cli wal-hook wiring — RESOLVED

Landed on branch `cli-wal-hook-wiring` (sqlink) + sqlite-loader-wit
`main` (ab3576f). The native side now mirrors the browser-side
substrate that `#436` shipped:

  - `sqlite-loader-wit/wit/guest.wit`: `manifest.has-wal-hook: bool`
    + `manifest.wal-hook-id: u64`; `hooked` world also exports
    `wal-hook` so a single loader linker dispatches all four hook
    surfaces.
  - `host/src/lib.rs`: `register_wal_hook` impl on the spi-loader
    trait + `dispatch_on_wal_hook` async dispatcher. Clears
    SQLite's default `wal_autocheckpoint` wal-hook before installing
    the extension's (avoids a UB drop of SQLite's internal pointer).
  - `cli/src/lib.rs`: parallel `if manifest.has_wal_hook { register_wal_hook(...) }`
    block alongside the existing `has-authorizer` / `has-update-hook` /
    `has-commit-hook` walks.
  - `extensions/hookprobe`: manifest declares `has-wal-hook: true` /
    `wal-hook-id: 42`.
  - `tests/extension-smoke/src/test_wal_hook.rs`: native mirror of
    `browser/tests/composed-wal-hook.spec.js`, asserts a
    `wal:42:main:<n>` event in hookprobe's drain log after a
    `PRAGMA journal_mode=WAL` + a handful of INSERTs.

### Substrate 2 (#439): wal-frames SPI interface  **LANDED 2026-06-23 (option A)**

Original plan called for TWO new SPI interfaces  `wal-frames` AND
`backup`. The design checkpoint at the head of this work picked
**option A**: drop the new `backup` interface and reuse the
existing `spi.serialize-db` (which has been there since the
initial spi cut, gated by `capability::spi`). Reduces the substrate
surface by ~30% with no functional loss: wal-archive's snapshot
cadence (default 24h) is the only `serialize-db` consumer the
extension needs.

What actually landed:

```wit
// sqlite-loader-wit/wit/host-spi.wit
interface wal-frames {
  use types.{sqlite-error};

  /// 32-byte WAL file header. None when journal_mode  wal or
  /// the WAL file doesn't exist yet.
  get-wal-header: func(db-name: string)
    -> result<option<list<u8>>, sqlite-error>;

  /// N frames starting at `start-frame` (1-based per SQLite's
  /// WAL format). Each frame is 24 + page_size bytes. Caller
  /// uses the WAL header to derive page_size first.
  read-frames: func(db-name: string, start-frame: u32, n-frames: u32)
    -> result<list<u8>, sqlite-error>;
}

// sqlite-loader-wit/wit/policy.wit  capability enum gained:
//   wal-frames,
```

Snapshot serialize: just call the existing
`sqlite:extension/spi.serialize-db(db-name)`  no separate
interface needed. The wal-archive snapshot cadence path is
`grant.contains(Capability::Spi) ? spi::serialize_db("main") : err`.

Other landed pieces:

- `Capability::WalFrames` variant on `sqlite-loader-wit/src/lib.rs`
  (the Rust source of truth shared across host / loader / native).
- `wal-frames` imported into every world (minimal, minimal-http,
  minimal-dns, stateful, lifecycle-aware, resolving, collating,
  authorizing, hooked, tabular, tabular-mutating, dotcmd-aware,
  wal-aware, hookprobe, full)  the per-shape host bindgens
  (`Hooked::add_to_linker`, `Authorizing::add_to_linker`, ...) all
  install the same `wal_frames::Host` impl via `with:` clauses, so
  the wal-archive-shaped extension can be dispatched against any
  world the host instantiates it as.
- Native host dispatcher (`host/src/lib.rs`): both methods open
  the on-disk `<sqlite3_db_filename(db-name)>-wal` sidecar with a
  `std::fs::read`, return the requested bytes. Fail-closed on
  `Capability::WalFrames` not granted at load time (SQLITE_PERM
  with a "wal-frames capability not granted" message).
- Browser-side stub in `sqlite-wasm/sqlite-lib`: returns the
  documented sentinel (None / SQLITE_NOTFOUND). #437 (vfs-tvm
  WAL support) has now landed, so the VFS underneath this stub
  is WAL-capable; replacing the stub with a real implementation
  that reads the WAL through the now-functional substrate is a
  follow-up. The WIT contract stays honored so a wal-archive-
  shaped extension can compose with sqlite-lib in the browser
  today, even though the live data isn't reachable yet.
- New test-bench scalars on hookprobe (`hookprobe_wal_header`,
  `hookprobe_read_frames`, `hookprobe_serialize_main`) and a
  native end-to-end smoke
  (`tests/extension-smoke/src/test_wal_frames.rs`, scenarios 1+2)
  asserting WAL magic + frame size + "SQLite format 3\0" header
  on the serialized snapshot.

The `backup` interface and `backup-aware` world that briefly
appeared in `sqlite-loader-wit` f66bdca were reverted in 522645e
once the design call landed.

### Substrate 3 (#440): host-resident `s3-base` SPI bridge  **LANDED**

Defines `sqlite:extension/s3-base@0.1.0` in `sqlite-loader-wit`,
mirroring `~/git/s3-wasm`'s `s3-base` interface (get/put/delete/
head/list/copy-object) record-for-record. Extensions import this
interface like they import spi/types/policy.

What landed:

- WIT contract (`sqlite-loader-wit/wit/host-spi.wit`): the
  `s3-base` interface plus the six S3 method signatures + the
  record types (s3-endpoint-config, s3-credentials, options,
  outputs) and the s3-error variant (including the extra
  `capability-not-granted` variant for the grant gate).
- `Capability::S3` in `sqlite-loader-wit/wit/policy.wit` + the
  Rust `Capability::S3` variant. Operator picks it via
  `--grant=s3` on `.load`.
- World widening: every world that hosts an extension (minimal,
  minimal-http, minimal-dns, stateful, lifecycle-aware,
  resolving, collating, authorizing, hooked, wal-aware,
  hookprobe, tabular, tabular-mutating, dotcmd-aware, full)
  imports `s3-base` so the per-shape host bindgens all install
  the same `s3_base::Host` impl via `with:` clauses.
- Native host bridge (`host/src/lib.rs` + `host/src/s3.rs`):
  **in-host** strategy. Each WIT method is dispatched via
  `tokio::spawn_blocking` to a synchronous routine that signs
  the request with the `aws-sigv4` Rust crate and sends it
  through `reqwest::blocking`. Fail-closed on
  `Capability::S3` not granted at load time (returns
  `S3Error::CapabilityNotGranted`).

  Implementation strategy decision: in-host beats composing the
  s3-wasm component into wasmtime because s3-wasm imports
  `wasi:http/outgoing-handler` + `aws:sigv4/{types,signer}`;
  satisfying those would require `wasmtime-wasi-http` + an
  additional aws-sigv4 component instance per loaded-extension
  Store, and the bookkeeping cost dwarfs the payoff for a
  sink-style SPI. The WIT contract is mirrored 1:1 so a future
  iteration can swap implementations without touching extension
  code.
- Browser-side stub in `sqlite-wasm/sqlite-lib`: each method
  returns `S3Error::Internal("not implemented; pending fetch+
  SigV4 polyfill bridge follow-up to #437")`. The wal-archive
  extension can't even start in the browser without WAL access
  (#437), so the browser-polyfill bridge is a follow-up rather
  than v1 scope.
- New test-bench scalars on hookprobe (`hookprobe_s3_put`,
  `hookprobe_s3_get`, `hookprobe_s3_list`, `hookprobe_s3_delete`)
  and a native end-to-end smoke (`tests/extension-smoke/src/
  test_s3_base.rs`, scenarios 1+2) that spins up a local mock
  S3 server (s3s + s3s-fs, filesystem-backed) on an ephemeral
  port and asserts a full PUT/GET/LIST/DELETE round-trip with
  byte equality.

Pattern reuses the dispatch-bridge work from #429/#432/#433/#436
and the wal-frames substrate from #439  one more flavor of
"host-resident capability surface" where extensions don't bring
their own credentials.

### Substrate 4 (#436): dispatch-bridge WAL hook

Already done. Browser-side only; #438 completes the native side.

### Substrate 5 (#441): cached hook + scalar dispatch  **LANDED 2026-06-23**

Native-only correctness fix. Pre-#441 the host re-instantiated the
loaded extension on every hook firing (one fresh wasmtime Store
per call), so guest-side `thread_local!` / `OnceLock` / `static
AtomicU64` state was wiped between callbacks. The browser side
shares one instance per page and was unaffected.

Wal-archive's design requires:

  * a `OnceLock<Mutex<RingBuffer>>` populated by
    `wal_archive_start({opts})` (a scalar call) and READ by every
    subsequent `on_wal_hook` firing,
  * a `static AtomicU64` segment-id counter incremented per
    wal-hook firing and observed across them,
  * a cached WAL header derived once and reused.

None of those can survive across firings unless the wasm Store
they live in is reused.

What landed:

- `CachedHooked` (per-extension `Arc<TokioMutex<Option<(Store,
  Hooked)>>>`) mirroring the existing `CachedMinimal` /
  `CachedTabular` / `CachedStateful` slots on `LoadedExtension`.
  Built lazily on the first hook firing or the first scalar call
  routed here, dropped when the extension is unloaded.
- `CachedAuthorizing` slot for symmetry (authorize callbacks now
  also share state across firings within the authorize world).
- `hooked_locked` / `authorizing_locked` helpers mirroring
  `minimal_locked` (lazy first instantiate, owned mutex guard,
  per-call budget refresh).
- `dispatch_on_update`, `dispatch_on_commit`,
  `dispatch_on_rollback`, `dispatch_on_wal_hook`,
  `dispatch_authorize` rewired to use the cached stores.
- `ScalarRoute::Hooked` route in `dispatch_scalar`: when the
  extension declares any hook (`has_update_hook ||
  has_commit_hook || has_wal_hook`), scalar dispatch routes
  through the SAME `cached_hooked` Store the hook dispatchers
  use. Cross-world coherence  the `wal_archive_start` scalar
  and the next `on_wal_hook` firing share one instance.
- `tests/extension-smoke/src/test_wal_hook.rs` tightened to
  assert `wal:42:main:<n>` events in `hookprobe_drain_log()`
  output, mirroring `composed-wal-hook.spec.js` in browser.

The `wal-aware` world has an identical export shape to `hooked`
(metadata + scalar-function + update-hook + commit-hook +
wal-hook); the host's `loaded_hooked::Hooked` bindgen and the
single `cached_hooked` slot service both worlds.

With this, the wal-archive substrate consumer work can proceed:
the extension can author against `wal-aware`, set state from
`wal_archive_start({opts})` scalar, and observe / mutate it
from the per-WAL-commit `on_wal_hook` callbacks  the native
side now matches the browser semantics.

### `dispatch-bridge` core methods (already done in #429/#432/#433/#436)

```wit
// sqlite-wasm/wit/library.wit (dispatch-bridge interface):
register-host-wal-hook: func(
    ext-name: string,
    hook-id: u64,
) -> result<_, sqlite-error>;

// sqlite-loader-wit/wit/spi.wit (dispatch interface):
wal-hook: func(
    ext-name: string,
    hook-id: u64,
    db-name: string,
    n-frames-in-wal: u32,
) -> s32;
```

Rust impl in sqlite-lib mirrors the scalar/aggregate/collation
pattern (#429, #432). ~30 lines WIT, ~40 lines Rust. **Half a
day.** Track as **#436 dispatch-bridge: WAL hook**.

## Storage layer: host-resident `s3-base` via SPI bridge (substrate #440)

After the design conversation on 2026-06-23, the extension does
NOT import `component:s3-wasm/s3-base@0.1.0` directly. Instead it
imports `sqlite:extension/s3-base@0.1.0` (the host-resident SPI
bridge). The host's impl bridges to `~/git/s3-wasm` natively or
to fetch+SigV4 in browser. See substrate #440 above.

The underlying capability  any S3-compatible service (AWS, R2,
Spaces, MinIO, Backblaze B2)  is unchanged from the original
design. What changed is the wire shape: extensions never see
S3 credentials; the host injects them at the bridge layer.

For reference, `~/git/s3-wasm` exposes:

```
interface s3-base {
  get-object(endpoint, credentials, bucket, key, opts) -> result<get-object-output, error>;
  put-object(endpoint, credentials, bucket, key, body, opts) -> result<put-object-output, error>;
  delete-object(endpoint, credentials, bucket, key) -> result<_, error>;
  head-object(endpoint, credentials, bucket, key) -> result<head-object-output, error>;
  list-objects(endpoint, credentials, bucket, opts) -> result<list-objects-output, error>;
  copy-object(endpoint, credentials, src-bucket, src-key, dst-bucket, dst-key, opts) -> result<copy-object-output, error>;
}

interface s3-aws {
  // storage-class, presigned-url, glacier-restore, ...
}
```

Works with **any S3-compatible service** (AWS, Cloudflare R2,
DigitalOcean Spaces, MinIO, Backblaze B2). HTTP via
`wasi:http/outgoing-handler` (polyfill provides in browser, wasmtime
provides natively). SigV4 signing via the `aws:sigv4` component
already wired.

**Design implication**: the original "injectable WIT storage-sink"
abstraction layer collapses. The extension just imports `s3-base`
directly. The composition (or runtime bindgen) wires s3-wasm in.
The browser doesn't need a separate JS S3 SDK.

The pluggable-sink WIT contract is still useful for **non-S3
backends** (in-memory test fake, IPFS, p2p sqlink-to-sqlink WAL
shipping). Keep as an OPTIONAL second interface, but s3-wasm is
the default and ships in v1.

## Future transport: `~/git/wireguard-wasm`

`~/git/wireguard-wasm/` is the project's WireGuard component
(x25519-dalek + chacha20poly1305 + blake2, full protocol stack).
Not needed for v1 (TLS via wasi:http handles transit security to
S3), but two future patterns benefit from it:

- **p2p sqlink-to-sqlink WAL shipping** over an encrypted tunnel
  to a private peer  no need for the destination to expose an
  HTTPS endpoint.
- **Shipping to private/internal storage** behind a network boundary
  without TLS termination at the edge.

Document as a future sink transport; not in v1 scope.

## Layout

```
extensions/wal-archive/
  Cargo.toml                  # imports s3-wasm via path / component dep
  wit/wal-archive.wit         # the extension's own surface
  src/lib.rs                  # WIT export glue
  src/wal.rs                  # WAL frame capture (uses register-host-wal-hook)
  src/segment.rs              # frame batching + compression
  src/snapshot.rs             # base snapshot via sqlite3_backup
  src/state.rs                # sidecar state in sink
  src/s3_sink.rs              # adapter from extension to s3-wasm imports
  src/restore.rs              # download + replay
```

**No `browser/` scaffolding.** The same `.component.wasm` works in
every scenario.

## WIT contract

```wit
package walarchive:replication@0.1.0;

interface replicator {
  use types.{sqlite-value};   // from sqlite:extension/types

  record options {
    snapshot-interval-seconds: u64,
    flush-bytes-threshold: u32,
    flush-ms-threshold: u32,
    compression: compression-codec,
    prefix: string,
    s3-endpoint: string,
    s3-bucket: string,
    s3-region: string,
    // s3-credentials sourced from wasi:cli/environment OR explicit opt
  }

  variant compression-codec { none, lz4, zstd }

  start: func(db-name: string, opts: options) -> result<_, string>;
  snapshot-now: func() -> result<_, string>;
  stop: func() -> result<_, string>;
  status: func() -> replication-status;
  restore: func(db-name: string, target-path: string, opts: options) -> result<_, string>;
}

world wal-archive-extension {
  import sqlite:extension/spi@0.1.0;          // serialize-db (snapshots)
  import sqlite:extension/types@0.1.0;
  import sqlite:extension/metadata@0.1.0;
  import sqlite:extension/wal-frames@0.1.0;   // substrate #439 (landed)
  import sqlite:extension/s3-base@0.1.0;      // substrate #440 (host-resident)
  import wasi:clocks/wall-clock@0.2.0;
  export replicator;
}
```

The earlier sketch imported a separate `sqlite:extension/backup`
interface here. Per substrate-#439's design checkpoint it was
dropped: the existing `spi.serialize-db` covers the one-shot
serialize the snapshot path needs (it shipped in the initial spi
cut and is already gated by `capability::spi`).

## Mechanics

```
on start(db_name, opts):
  - register WAL hook via spi-loader.register-wal-hook
    (routes through dispatch-bridge.register-host-wal-hook)
  - load sidecar state from s3 (s3-base::get-object)
  - catch up any WAL frames newer than state.last_uploaded_frame

on wal_hook(db, name, n_pages):
  - read frames from <db>-wal via wasi:filesystem
  - append to in-memory ring buffer
  - flush() when buffer crosses threshold OR enough wall-clock elapsed

flush():
  - segment_id = next id
  - bytes = compress(buffer)
  - s3-base::put-object(endpoint, credentials, bucket,
                        "{prefix}{db}/wal/{segment_id:020}.{ext}", bytes)
  - update state.json via s3-base::put-object
  - buffer.clear()

snapshot loop (every snapshot_interval_seconds):
  - sqlite3_backup_init  memory db  serialize  compress
  - s3-base::put-object snapshot + update latest pointer
  - garbage-collect WAL segments older than the snapshot frame
```

Restore reverses: `s3-base::get-object` the latest snapshot,
decompress to OPFS / local file, `s3-base::list-objects` WAL
segments past the snapshot frame, append to `<target>-wal`, open
the db (SQLite replays the WAL).

## Per-scenario invocation

**Browser**:
```js
const db = await openDatabase()
await db.loadExtension('wal-archive', walArchiveBytes)
await db.exec(`SELECT wal_archive_start('user-42', json_object(
  'snapshot_interval_seconds', 86400,
  's3_endpoint', 'https://s3.amazonaws.com',
  's3_bucket', 'my-app-backups',
  's3_region', 'us-east-1',
  'prefix', 'user-42/'
))`)
```

**Native**:
```bash
sqlink --db user.sqlite -c "
  .load wal_archive.component.wasm
  SELECT wal_archive_start('user', json_object(
    's3_endpoint', 'https://s3.amazonaws.com',
    's3_bucket', 'my-app-backups',
    's3_region', 'us-east-1',
    'prefix', 'user/'));
"
# or as a flag:
sqlink --db user.sqlite --backup 's3://my-app-backups/user/'
```

**Edge / serverless**: same `.wasm`, same SQL.

## Compression: lz4_flex first

Pure-Rust, no_std-friendly, ~30 KB code size. zstd as a follow-up
when ratio matters more than size.

## Effort estimate

| Piece | Effort |
|---|---|
| **Substrate**: dispatch-bridge WAL hook (#436) | ½ day |
| WIT contract + bindings | ½ day |
| WAL frame capture (consume the WAL file via wasi:filesystem) | 2 days |
| Compression + segment shipping (consume s3-wasm) | 1 day |
| Sidecar state in sink + crash-recovery catch-up | 1½ days |
| Snapshot path via `sqlite3_backup_*` | 2 days |
| Restore path | 1½ days |
| `sqlink --backup` CLI flag | 1 day |
| Per-scenario integration tests (browser via Playwright + native via cargo test + MinIO for the s3 backend) | 3 days |

**Total: ~12 days,  2.5 weeks for v1.**

## Dependencies

- `dispatch-bridge.register-host-wal-hook` (substrate, ~½ day).
- `~/git/s3-wasm` at its current shape (already exists; no
  changes needed for v1).
- Composed cli + sqlite-lib browser runtime (done, commit
  `f7530b0`).
- Extension catalog dispatch (done, #429/#432).

## Out of scope (v1)

- Encryption: deferred. Layer either underneath the sink (the S3
  endpoint encrypts in transit + at rest via SSE-S3 / SSE-KMS) or
  above the codec (a future `crypted-lz4` variant).
- `wal-archive validate` byte-identity check (Litestream has the
  equivalent)  ship later.
- Multipart S3 upload  v1 segments are bounded by
  `flush-bytes-threshold` (default 64 KiB). Bump if needed.
- Multi-region replication  v1 ships to one sink; layering N is
  straightforward later.
- p2p sqlink-to-sqlink WAL shipping over wireguard-wasm 
  documented above; needs a separate sink WIT contract. Pull when
  there's demand.

## What changes vs the original `PLAN-browser-litestream.md`

- **Scope**: browser-only  all scenarios.
- **Name**: `litestream`  `wal-archive`. Litestream + Ben Johnson
  credited in README.
- **Storage**: write our own S3 sink  import `~/git/s3-wasm`.
- **Sink abstraction**: WIT-defined storage-sink as the only path 
  s3-wasm as default, optional pluggable sink trait for alternates.
- **Layout**: `browser/` + `extensions/`  just
  `extensions/wal-archive/` (one .wasm everywhere).
- **Future transport**: wireguard-wasm noted for p2p / private-
  storage variants.
- **Estimate**: 2 weeks  ~2.5 weeks (gain s3-wasm dep, lose to
  scenario testing).

The old `PLAN-browser-litestream.md` should be deleted on merge
of this plan.

## Sequencing

1. Land **#436 WAL hook** as a small precursor (½ day).
2. Build the extension end-to-end (per the layout above).
3. Per-scenario integration tests: Playwright (browser),
   `cargo test -p extension-smoke --features wal-archive` (native),
   maybe a small Cloudflare Workers reproducer to prove edge.
4. CLI flag (`sqlink --backup `) ships as a thin wrapper.

## Credits

Design heavily inspired by **[Litestream](https://litestream.io/)**
by **[Ben Johnson](https://github.com/benbjohnson)**. WAL-segment
shipping + base-snapshot + point-in-time recovery semantics all
come from Litestream; this extension is a separate implementation
in a different runtime model (in-process inside a WASM component
rather than as a separate Go daemon). Credit to land in the
project README's Acknowledgements section alongside Simon Willison
+ sqlite-utils.

## References

- [Litestream documentation](https://litestream.io/)
- [Litestream WAL frame format](https://github.com/benbjohnson/litestream/blob/main/wal.go)
- [SQLite WAL format](https://www.sqlite.org/walformat.html)
- [`sqlite3_wal_hook` docs](https://www.sqlite.org/c3ref/wal_hook.html)
- `~/git/s3-wasm` (the local S3-compatible component we consume)
- `~/git/wireguard-wasm` (future transport option)
- Substrate landings: dispatch-bridge for scalars (#429), aggregates
  + collations (#432); WAL hook will be #436.
