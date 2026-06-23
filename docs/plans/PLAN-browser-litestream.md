# Plan: Browser-side Litestream as a sqlink extension

## Goal

Implement continuous WAL replication for OPFS-resident SQLite databases,
running entirely in the browser as a sqlink WASM extension. Same shape as
[Litestream](https://github.com/benbjohnson/litestream) but in-process
inside the browser, with an injectable storage sink (S3 / R2 / fake / etc.)
provided from the JS host side.

## Why this and not just running upstream Litestream

Upstream Litestream is a separate Go process that fsnotifies the WAL file
and ships segments to object storage. In the browser scenario there is no
"separate process" — the wasm runtime + the JS host are the whole world.
A litestream-shaped extension is the only way to get continuous backup
of an OPFS database to S3 from a web app without server-side coordination.

The native and `sqlink-loader.so` scenarios already have a perfectly good
option: run upstream Litestream alongside the host process. Don't
reimplement that.

Logical replication via the session extension (Stage 6) is a different
primitive: changeset-based, schema-aware, transport-agnostic. It's
strictly more portable for "ship a diff to another database" use cases.
This plan is the WAL-segment counterpart — useful when you want
byte-identical recovery, sub-second RPO, and arbitrary point-in-time
restore.

## Layout

```
extensions/litestream/
  Cargo.toml
  wit/litestream.wit               # WIT contract
  src/lib.rs                       # extension code
  src/wal.rs                       # WAL frame parsing + capture
  src/snapshot.rs                  # base-snapshot via sqlite3_backup_*
  src/state.rs                     # sidecar state read/write
  src/codec.rs                     # lz4 (+ later zstd) compression
browser/
  src/storage-sink-s3.js           # JS impl of the WIT storage-sink
  src/storage-sink-fake.js         # test fake (in-memory)
  tests/litestream.spec.js         # Playwright roundtrip test
```

## WIT contract

```wit
package litestream:replication@0.1.0;

interface storage-sink {
  put:    func(key: string, bytes: list<u8>) -> result<_, string>;
  get:    func(key: string)                  -> result<list<u8>, string>;
  list:   func(prefix: string)               -> result<list<string>, string>;
  delete: func(key: string)                  -> result<_, string>;
}

variant compression-codec {
  none,
  lz4,
  zstd,
}

record replication-options {
  /// How often to roll a fresh base snapshot. Default 24h.
  snapshot-interval-seconds: u64,
  /// Flush WAL buffer to sink at this size.
  flush-bytes-threshold: u32,
  /// ...or at this many ms since last flush.
  flush-ms-threshold: u32,
  compression: compression-codec,
  /// Object-key prefix in storage.
  prefix: string,
}

record replication-status {
  running: bool,
  last-flush-ms-ago: u64,
  last-snapshot-ms-ago: u64,
  pending-bytes: u32,
  total-bytes-shipped: u64,
  total-segments-shipped: u32,
  last-error: option<string>,
}

interface replicator {
  start:        func(db-name: string, opts: replication-options) -> result<_, string>;
  snapshot-now: func() -> result<_, string>;
  stop:         func() -> result<_, string>;
  status:       func() -> replication-status;
  restore:      func(db-name: string, target-path: string) -> result<_, string>;
}

world replicator-world {
  import storage-sink;
  import sqlite:extension/spi@0.1.0;
  import sqlite:extension/types@0.1.0;
  import wasi:clocks/wall-clock@0.2.0;
  import wasi:filesystem/types@0.2.0;
  export replicator;
}
```

The `storage-sink` is imported, not built in: the JS host (or another
WASM impl) provides it. This is what makes the design portable across
S3 / R2 / IPFS / GCS / in-memory-for-tests.

## Mechanics

### Replication loop

```
on start(db_name, opts):
  - install sqlite3_wal_hook(db, on_wal_commit)
  - install sqlite3_commit_hook(db, on_commit) for ordering
  - load sidecar state from sink: state["{prefix}{db_name}/state.json"]
  - catch-up: any WAL frames beyond state.last_uploaded_frame
    get queued for immediate upload
  - schedule snapshot-loop timer (snapshot_interval_seconds)
  - set state.running = true

on_wal_commit(db, name, n_pages):
  - read frames from <db>-wal via wasi:filesystem (offset is known
    from the WAL header at the last commit boundary)
  - append to in-memory buffer
  - if buffer.len >= flush_bytes_threshold or
       wall_clock - last_flush >= flush_ms_threshold:
       flush()

flush():
  - segment_id = next monotonic id (atomic counter in sidecar state)
  - bytes = compress(buffer, opts.compression)
  - sink.put("{prefix}{db_name}/wal/{segment_id:020}.{ext}", bytes)?
  - state.last_uploaded_frame = current_wal_frame
  - sink.put("{prefix}{db_name}/state.json", state.to_bytes())?
  - buffer.clear()
  - last_flush = wall_clock

snapshot_loop (timer):
  every snapshot_interval_seconds:
    snapshot_now()

snapshot_now():
  - sqlite3_backup_init(target=memory_db, source=user_db)
  - sqlite3_backup_step until done
  - bytes = compress(memory_db_file)
  - sink.put("{prefix}{db_name}/snapshots/{ts}.snap.{ext}", bytes)?
  - sink.put("{prefix}{db_name}/snapshots/latest.snap.{ext}", bytes)
  - state.last_snapshot_frame = current_wal_frame
  - schedule GC: WAL segments < state.last_snapshot_frame can be
    deleted after a grace period (1h default)
```

### Restore loop

```
restore(db_name, target_path):
  - latest_snap = sink.get("{prefix}{db_name}/snapshots/latest.snap.{ext}")
  - decompress latest_snap -> target_path in OPFS
  - segments = sink.list("{prefix}{db_name}/wal/")
  - segments_after_snap = filter(segments, id > state.last_snapshot_frame)
  - sort by id
  - for each segment: decompress, append frames to <target_path>-wal
  - opening the db normally now replays the WAL (SQLite does this)
```

## Key design decisions

### Compression: `lz4_flex` first

Pure-Rust, no_std-friendly, predictable wasm code size (~30 KB). Same
trade Litestream made for similar reasons. `zstd` is a follow-up for
better ratios at a ~200 KB code-size hit.

### State sidecar lives in the sink, not local

Litestream uses a local sidecar SQLite db. We put state in the sink
itself (`{prefix}{db}/state.json`) so the browser can restore from
any device without local state. Atomic replacement via S3 conditional
PUT / R2 etag.

### Checkpoint coordination is trivial in-process

Litestream's daemon keeps a second connection open to prevent SQLite
from auto-checkpointing before it has copied. In-process means we
control the writer:

```sql
PRAGMA journal_mode = WAL;
PRAGMA wal_autocheckpoint = 0;
```

Issue `PRAGMA wal_checkpoint(TRUNCATE)` only after a successful
base-snapshot upload. No second-connection trick.

### Snapshot frequency: configurable, default 24h

Shorter intervals → smaller WAL replay cost but more snapshot upload
overhead. 24h is the litestream default and the right starting point.

### Garbage collection: snapshot-relative + grace period

When snapshot N lands, schedule deletion of WAL segments with frame
index < N's boundary after `gc_grace_seconds` (default 3600). Grace
covers in-flight readers / restores.

### Multi-tab safety: leader election

OPFS doesn't single-writer across tabs in all browsers. Use a leader-
election lock on a sidecar OPFS file (`{db}-lease`). Same pattern as
Vlcn / cr-sqlite. Non-leader tabs proxy writes to the leader; the
leader is the only one shipping WAL.

### Failure semantics

If the browser tab crashes mid-flush: on next start, the sidecar state
is older than the actual WAL frame. The catch-up loop ships the missing
range before serving live writes. No data loss except for unflushed
in-memory buffer (bounded by `flush_bytes_threshold` + page size).

If the sink is unreachable: buffer holds WAL bytes; if the buffer
overflows a configurable cap (default 64 MiB), the extension surfaces an
error via `replicator.status()`. Apps can react by pausing writes,
backing off, or alerting the user.

## What's distinct from upstream Litestream

| Upstream Litestream | This extension |
|---|---|
| Separate Go process | In-process inside the WASM cli |
| fsnotify watches WAL file | `wal_hook` callback (deterministic, no polling) |
| Hard-coded S3 / GCS / Azure / SFTP / file | Injectable WIT `storage-sink` |
| Survives app crash | Dies with the tab; resumes from sink-side state |
| Single platform per binary | Same `.wasm` runs in browser + Node + native + workers |
| Local sidecar state DB | Sink-side state JSON (device-independent) |

## Effort estimate

| Piece | Effort |
|---|---|
| WIT interface + bindings | 0.5 day |
| WAL frame capture + parse | 2 days |
| Compression + sink protocol | 1 day |
| Sidecar state + crash-recovery catch-up | 1.5 days |
| Snapshot path via `sqlite3_backup_*` | 2 days |
| Restore path | 1.5 days |
| JS-side S3 sink reference impl | 1 day |
| Tests: mocked sink, roundtrip, failure modes | 4 days |

**Total: ~2 weeks for v1.**

## Dependencies

- Composed `cli + sqlite-lib` component working in browser end-to-end.
  Currently blocked by #422 (pcache cold-tier runtime trap inside the
  composed runtime).
- `wal_hook` exposed through `sqlite:extension/spi`. May already be
  there from Stage 6's session/changeset work; verify before starting.
- `sqlite3_backup_*` accessible from a WASM extension via SPI. Same
  caveat.
- A reliable OPFS-resident WAL file for the wasi-filesystem reader to
  point at. The composed component's vfs-tvm currently routes through
  OPFS for db files — verify the `-wal` sibling lands there too.

## Sequencing

Best done as a focused 2-week sprint **after** Path 3's pcache trap
(#422) closes out. Don't start until the composed runtime instantiates
end-to-end and can run basic SELECTs in browser without trapping.

## Out of scope (v1)

- Encryption: deferred. WAL bytes go to the sink as-compressed-only.
  Encryption can layer underneath the sink (the JS S3 sink encrypts
  before PUT) or above the codec (a future `crypted-lz4` variant).
- Validation / fsck: deferred. Litestream proper has `litestream
  validate` to compare a restored DB against the source byte-by-byte;
  we can ship the same later.
- Async / streaming uploads: v1 buffers a segment in memory, then
  PUT-uploads atomically. Multipart upload comes later if segment
  sizes warrant it.
- Multi-region replication: deferred. v1 ships to one sink; layering
  multiple sinks is straightforward in the JS host.

## References

- [Litestream documentation](https://litestream.io/)
- [Litestream WAL frame format](https://github.com/benbjohnson/litestream/blob/main/wal.go)
- SQLite WAL format: <https://www.sqlite.org/walformat.html>
- `sqlite3_wal_hook` docs: <https://www.sqlite.org/c3ref/wal_hook.html>
- `sqlite3_backup_*` docs: <https://www.sqlite.org/c3ref/backup_finish.html>
