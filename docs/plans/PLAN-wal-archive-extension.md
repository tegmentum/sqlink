# Plan: `wal-archive` extension  continuous WAL backup for sqlink

## Status (2026-06-23)

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

### Substrate 1 (#438): native cli wal-hook wiring

`#436` wired the dispatch-bridge browser-side only. The native cli
(`cli/src/lib.rs`) registers six hook kinds during extension load
but no `register_wal_hook` call. Add it + a `has-wal-hook` field
on the manifest record in `sqlite-loader-wit/wit/guest.wit`
(referenced in a comment but never declared). ~1 day.

### Substrate 2 (#439): wal-frames + backup SPI interfaces

Two new SPI interfaces in `sqlite-loader-wit/wit/`:

```wit
package sqlite:extension@0.1.0;

interface wal-frames {
  use types.{sqlite-error};

  /// 32-byte WAL file header. None when journal_mode  wal.
  get-wal-header: func(db-name: string)
    -> result<option<list<u8>>, sqlite-error>;

  /// N frames starting at `start-frame` (1-based per SQLite's
  /// WAL format). Each frame is 24 + page_size bytes. Caller
  /// uses the WAL header to derive page_size first.
  read-frames: func(db-name: string, start-frame: u32, n-frames: u32)
    -> result<list<u8>, sqlite-error>;
}

interface backup {
  use types.{sqlite-error};

  /// One-shot serialize. Briefly holds the write lock during
  /// sqlite3_serialize(). Allocates the full db in wasm memory 
  /// fine for the wal-archive snapshot cadence (default 24h).
  /// Incremental sqlite3_backup_* + streaming variants are v2.
  serialize-db: func(db-name: string) -> result<list<u8>, sqlite-error>;
}
```

Extensions request these interfaces in their world; host serves
them via the existing SPI dispatcher. Capability gating: each
interface separate so an extension that only needs backup doesn't
have to be granted wal-frame access. ~1-2 days.

### Substrate 3 (#440): host-resident `s3-base` SPI bridge

Define `sqlite:extension/s3-base@0.1.0` in `sqlite-loader-wit`,
mirroring `~/git/s3-wasm`'s `s3-base` interface (get/put/delete/
head/list/copy-object). Extensions import this interface like
they import spi/types/policy. The host provides the impl:

- **Native (scenarios 1+2 + sqlink-loader.so)**: bridges to
  `~/git/s3-wasm` via wasmtime (host instantiates s3-wasm once
  and routes extension calls to it).
- **Browser** (when #437 vfs-tvm WAL lands): bridges to
  `fetch` + a SigV4 JS impl via the polyfill. Cheaper than
  running s3-wasm in JS-land.

Three reasons this beats build-time `wac plug` baking or runtime
composition:

1. **Security model**: extensions don't bring their own
   credentials. The host injects credentials via the bridge.
   AWS keys / R2 tokens live with the host, not in extension
   code. Same shape as policy/grants for capability-restricted
   operations.
2. **Pattern reuse**: mirrors the dispatch-bridge work from
   #429/#432/#433/#436  one more flavor of "host-resident
   capability surface."
3. **Per-platform flexibility**: native can use the heavyweight
   s3-wasm component directly; browser can route to fetch
   (smaller, native to the platform).

~2-3 days (including the per-platform impl).

### Substrate 4 (#436): dispatch-bridge WAL hook

Already done. Browser-side only; #438 completes the native side.

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
  import sqlite:extension/spi@0.1.0;
  import sqlite:extension/types@0.1.0;
  import sqlite:extension/metadata@0.1.0;
  import sqlite:extension/wal-frames@0.1.0;   // substrate #439
  import sqlite:extension/backup@0.1.0;       // substrate #439
  import sqlite:extension/s3-base@0.1.0;      // substrate #440 (host-resident)
  import wasi:clocks/wall-clock@0.2.0;
  export replicator;
}
```

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
