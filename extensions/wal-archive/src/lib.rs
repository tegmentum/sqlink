//! `wal-archive`  continuous WAL-frame shipping + on-demand
//! snapshots for sqlink. Inspired by Litestream (Ben Johnson);
//! credited in the project README's Acknowledgements section.
//!
//! Stage 1 (this commit) lands the WIT contract + the manifest
//! that wires `has-wal-hook: true` + the three substrate
//! capabilities (Spi + WalFrames + S3) and stubs out the five
//! scalar entry points so the cli can `.load` the extension and
//! call them. Subsequent stages fill in frame capture, segment
//! shipping, sidecar state, snapshots, restore, smoke tests.
//!
//! Scalar surface (all string-args + integer-return):
//!
//!   wal_archive_start(db TEXT, opts_json TEXT) -> INTEGER
//!     Begin shipping. `db` is the SQLite schema name (typically
//!     "main"); `opts_json` is a JSON object with the keys laid
//!     out in `ArchiveOptions::parse`. Returns 0 on success or
//!     SQLITE_ERROR on a parse / state failure.
//!
//!   wal_archive_stop() -> INTEGER
//!     Stop shipping; subsequent wal-hook firings are no-ops.
//!     Returns 0.
//!
//!   wal_archive_snapshot_now() -> INTEGER
//!     Take an on-demand full snapshot via spi.serialize-db,
//!     compress + ship to S3, then GC older WAL segments.
//!     Returns the snapshot size in bytes.
//!
//!   wal_archive_status() -> TEXT
//!     JSON status blob (started / bookmark / segment counter /
//!     bytes-in-buffer). For diagnostics; tests assert on
//!     individual fields.
//!
//!   wal_archive_restore(db TEXT, target_path TEXT, opts_json TEXT)
//!       -> INTEGER
//!     Pull the latest snapshot from S3 to `target_path`, then
//!     append the WAL segments past the snapshot frame to
//!     `<target_path>-wal`. Returns the number of WAL frames
//!     replayed.
//!
//! Why the `wal-aware` world (not a wal-archive-specific one):
//! `wal-aware` already exports `metadata + scalar-function +
//! update-hook + commit-hook + wal-hook` and imports `spi +
//! wal-frames + s3-base + types + session + logging + config`,
//! which matches what wal-archive needs. The host's
//! `cached_hooked` slot services both `hooked` and `wal-aware`
//! worlds (#441), so scalar dispatch and wal-hook dispatch share
//! one Store per extension — the cross-firing state in
//! `STATE` actually persists.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "wal-aware",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::commit_hook::Guest as CommitHookGuest;
    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::update_hook::Guest as UpdateHookGuest;
    use bindings::exports::sqlite::extension::wal_hook::Guest as WalHookGuest;
    use bindings::sqlite::extension::policy::Capability;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue, UpdateOperation};
    use bindings::sqlite::extension::wal_frames;

    /// Distinct from hookprobe's WAL_HOOK_ID (42) so a single
    /// connection could in principle host both extensions. The
    /// host echoes this id back to `wal-hook.on-wal-hook` so the
    /// guest can disambiguate which of its hooks fired.
    const WAL_HOOK_ID: u64 = 1;

    // ---- scalar function ids ----
    const FID_START: u64 = 1;
    const FID_STOP: u64 = 2;
    const FID_SNAPSHOT_NOW: u64 = 3;
    const FID_STATUS: u64 = 4;
    const FID_RESTORE: u64 = 5;

    struct Ext;

    thread_local! {
        /// Per-Store extension state. Populated by
        /// `wal_archive_start` and read by every subsequent
        /// wal-hook firing on the same Store. The #441 cached-
        /// store unification is what makes this work: scalar +
        /// hook dispatch share one Store per extension, so the
        /// thread_local survives across firings.
        ///
        /// `None` until `wal_archive_start` runs; subsequent
        /// `wal_archive_stop` flips `started=false` but keeps
        /// the state record around so `wal_archive_status` can
        /// still report the bookmark + segment counter.
        static STATE: RefCell<Option<ArchiveState>> = const { RefCell::new(None) };
    }

    /// In-memory state shared between `wal_archive_start`, the
    /// wal-hook callback, and the snapshot / restore scalars.
    /// Stage 1 keeps fields minimal; later stages add the ring
    /// buffer + last_flush_ts_ms + cached WAL header.
    struct ArchiveState {
        /// True while we should drain frames on every wal-hook
        /// firing. `wal_archive_stop` flips this off.
        started: bool,
        /// SQLite schema name to track (typically "main").
        db_name: String,
        /// Operator-supplied options (S3 endpoint, bucket, etc).
        opts: ArchiveOptions,
        /// Frame-id bookmark: the highest WAL frame we've
        /// successfully shipped to S3. Hook firings drain frames
        /// `last_uploaded_frame + 1 ..= n_frames_in_wal`.
        last_uploaded_frame: u64,
        /// Counter for the next segment file's name. Increments
        /// after every successful flush.
        next_segment_id: u64,
        /// Highest frame id covered by the latest snapshot. WAL
        /// segments with id <= this are eligible for GC.
        last_snapshot_frame: u64,
        /// 32-byte WAL file header (magic + page_size + ...).
        /// Cached after the first wal-frames::get-wal-header
        /// call so we don't re-read it on every flush.
        wal_header: Option<Vec<u8>>,
        /// Pending compressed frames awaiting a flush. Empty
        /// after each successful upload.
        buffer: Vec<u8>,
        /// Wall-clock timestamp (millis since epoch) of the
        /// last successful flush. Drives the time-threshold
        /// flush trigger.
        last_flush_ts_ms: u64,
    }

    /// Operator-supplied options. Parsed from the `opts_json`
    /// argument to `wal_archive_start`. Field naming matches
    /// JSON shape:
    ///
    /// ```json
    /// {
    ///   "s3_endpoint": "https://s3.amazonaws.com",
    ///   "s3_bucket": "my-app-backups",
    ///   "s3_region": "us-east-1",
    ///   "s3_access_key_id": "AKIA...",
    ///   "s3_secret_access_key": "...",
    ///   "prefix": "user-42/",
    ///   "flush_bytes_threshold": 65536,
    ///   "flush_ms_threshold": 1000,
    ///   "snapshot_interval_seconds": 86400,
    ///   "path_style": true
    /// }
    /// ```
    struct ArchiveOptions {
        s3_endpoint: String,
        s3_bucket: String,
        s3_region: String,
        s3_access_key_id: String,
        s3_secret_access_key: String,
        /// S3 key prefix; should end with `/`. Defaults to empty
        /// string if absent.
        prefix: String,
        /// Flush whenever the buffer crosses this many bytes.
        /// Default: 64 KiB.
        flush_bytes_threshold: u32,
        /// Flush whenever this many millis have elapsed since
        /// the last flush, even if the buffer is below the
        /// byte threshold. Default: 1000 ms.
        flush_ms_threshold: u32,
        /// Interval between automatic snapshots in seconds. v1
        /// honors this only via on-demand `snapshot_now`; the
        /// timer-driven variant is a v2 follow-up. Default:
        /// 86400 (24 h).
        snapshot_interval_seconds: u64,
        /// Path-style addressing (`http://endpoint/bucket/key`
        /// vs subdomain-style `http://bucket.endpoint/key`).
        /// Required for most localhost / MinIO / mock fixtures;
        /// AWS supports both. Default: true.
        path_style: bool,
    }

    impl ArchiveOptions {
        fn parse(json: &str) -> Result<Self, String> {
            let v: serde_json::Value = serde_json::from_str(json)
                .map_err(|e| format!("wal_archive: opts JSON parse error: {e}"))?;
            let obj = v
                .as_object()
                .ok_or_else(|| "wal_archive: opts must be a JSON object".to_string())?;
            let get_str = |k: &str| -> Result<String, String> {
                obj.get(k)
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
                    .ok_or_else(|| format!("wal_archive: missing string field `{k}`"))
            };
            let get_opt_str = |k: &str| -> String {
                obj.get(k)
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            };
            let get_u64 = |k: &str, default: u64| -> u64 {
                obj.get(k).and_then(|x| x.as_u64()).unwrap_or(default)
            };
            let get_u32 = |k: &str, default: u32| -> u32 {
                obj.get(k)
                    .and_then(|x| x.as_u64())
                    .map(|n| n.min(u32::MAX as u64) as u32)
                    .unwrap_or(default)
            };
            let get_bool = |k: &str, default: bool| -> bool {
                obj.get(k).and_then(|x| x.as_bool()).unwrap_or(default)
            };
            Ok(Self {
                s3_endpoint: get_str("s3_endpoint")?,
                s3_bucket: get_str("s3_bucket")?,
                s3_region: get_str("s3_region")?,
                s3_access_key_id: get_str("s3_access_key_id")?,
                s3_secret_access_key: get_str("s3_secret_access_key")?,
                prefix: get_opt_str("prefix"),
                flush_bytes_threshold: get_u32("flush_bytes_threshold", 65536),
                flush_ms_threshold: get_u32("flush_ms_threshold", 1000),
                snapshot_interval_seconds: get_u64("snapshot_interval_seconds", 86_400),
                path_style: get_bool("path_style", true),
            })
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "wal-archive".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    ScalarFunctionSpec {
                        id: FID_START,
                        name: "wal_archive_start".to_string(),
                        num_args: 2,
                        // DIRECT_ONLY so the planner doesn't try to
                        // evaluate this in a recursive trigger / view
                        // context  side-effecting external I/O.
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_STOP,
                        name: "wal_archive_stop".to_string(),
                        num_args: 0,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_SNAPSHOT_NOW,
                        name: "wal_archive_snapshot_now".to_string(),
                        num_args: 0,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_STATUS,
                        name: "wal_archive_status".to_string(),
                        num_args: 0,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_RESTORE,
                        name: "wal_archive_restore".to_string(),
                        num_args: 3,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                // We don't authorize or subscribe to update/commit
                // hooks — only the wal-hook  but the cached-store
                // unification routes hook-bearing extensions
                // (`has_*_hook`) through the same Store, so flipping
                // wal-hook on is enough.
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: true,
                wal_hook_id: WAL_HOOK_ID,
                dot_commands: alloc::vec![],
                // `Spi` backs spi.serialize-db (snapshot path).
                // `WalFrames` backs wal-frames.{get-wal-header,
                // read-frames} (hook path). `S3` backs every
                // s3-base.* call (segment + state + snapshot
                // shipping). The host's policy gate refuses the
                // load if any are missing from the operator's
                // --grant list.
                declared_capabilities: alloc::vec![
                    Capability::Spi,
                    Capability::WalFrames,
                    Capability::S3,
                ],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_START => start(args),
                FID_STOP => stop(),
                FID_SNAPSHOT_NOW => snapshot_now(),
                FID_STATUS => status(),
                FID_RESTORE => restore(args),
                _ => Err(format!("wal_archive: unknown func_id={func_id}")),
            }
        }
    }

    fn pop_text(
        it: &mut alloc::vec::IntoIter<SqlValue>,
        field: &str,
    ) -> Result<String, String> {
        match it.next() {
            Some(SqlValue::Text(s)) => Ok(s),
            Some(other) => Err(format!(
                "wal_archive: arg `{field}` must be TEXT, got {other:?}"
            )),
            None => Err(format!("wal_archive: missing arg `{field}`")),
        }
    }

    /// Stage 1 stub: validate args, parse opts, install state,
    /// return 0. No actual shipping yet — the wal-hook handler
    /// is a no-op until Stage 2 wires frame capture.
    fn start(args: Vec<SqlValue>) -> Result<SqlValue, String> {
        let mut it = args.into_iter();
        let db_name = pop_text(&mut it, "db_name")?;
        let opts_json = pop_text(&mut it, "opts_json")?;
        let opts = ArchiveOptions::parse(&opts_json)?;
        STATE.with(|s| {
            *s.borrow_mut() = Some(ArchiveState {
                started: true,
                db_name,
                opts,
                last_uploaded_frame: 0,
                next_segment_id: 0,
                last_snapshot_frame: 0,
                wal_header: None,
                buffer: Vec::new(),
                last_flush_ts_ms: 0,
            });
        });
        Ok(SqlValue::Integer(0))
    }

    fn stop() -> Result<SqlValue, String> {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.started = false;
            }
        });
        Ok(SqlValue::Integer(0))
    }

    /// Stage 1 stub. Real impl lands in Stage 5.
    fn snapshot_now() -> Result<SqlValue, String> {
        Ok(SqlValue::Integer(0))
    }

    /// Diagnostic JSON status. Tests assert on individual fields.
    fn status() -> Result<SqlValue, String> {
        let json = STATE.with(|s| -> String {
            let guard = s.borrow();
            match guard.as_ref() {
                None => "{\"started\":false}".to_string(),
                Some(state) => format!(
                    "{{\"started\":{},\"db_name\":\"{}\",\"last_uploaded_frame\":{},\"next_segment_id\":{},\"last_snapshot_frame\":{},\"buffer_bytes\":{}}}",
                    state.started,
                    state.db_name,
                    state.last_uploaded_frame,
                    state.next_segment_id,
                    state.last_snapshot_frame,
                    state.buffer.len(),
                ),
            }
        });
        Ok(SqlValue::Text(json))
    }

    /// Stage 1 stub. Real impl lands in Stage 6.
    fn restore(args: Vec<SqlValue>) -> Result<SqlValue, String> {
        let mut it = args.into_iter();
        let _db_name = pop_text(&mut it, "db_name")?;
        let _target_path = pop_text(&mut it, "target_path")?;
        let _opts_json = pop_text(&mut it, "opts_json")?;
        Ok(SqlValue::Integer(0))
    }

    impl UpdateHookGuest for Ext {
        // We don't subscribe to update events; the manifest sets
        // `has_update_hook: false` so the host doesn't wire a
        // trampoline, but the world's export signature requires
        // the guest to define the function regardless. It's never
        // called.
        fn on_update(
            _operation: UpdateOperation,
            _database: String,
            _table: String,
            _rowid: i64,
        ) {
        }
    }

    impl CommitHookGuest for Ext {
        // Same  manifest says no, world says yes-by-export-shape.
        fn on_commit() -> bool {
            true
        }
        fn on_rollback() {}
    }

    impl WalHookGuest for Ext {
        /// Drain newly-appended WAL frames since the bookmark
        /// into the in-memory buffer. Stage 3 adds the flush
        /// trigger that compresses + ships the buffer to S3.
        ///
        /// Returns SQLITE_OK (0) so the calling SQL statement
        /// proceeds even if frame capture fails  the design
        /// favors not blocking writes over guaranteed shipping.
        /// Failures land in `STATE` so `wal_archive_status` can
        /// surface them, and the next firing will retry from
        /// the same bookmark.
        ///
        /// IMPORTANT: this runs on the SAME wasmtime Store that
        /// served the `wal_archive_start` scalar that set up
        /// `STATE` (#441 cached-store unification). Without that
        /// fix the thread_local would be wiped between firings
        /// and the design would not work.
        fn on_wal_hook(
            _hook_id: u64,
            db_name: String,
            n_frames_in_wal: u32,
        ) -> i32 {
            let _ = drain_frames(&db_name, n_frames_in_wal);
            0
        }
    }

    /// Read frames `[last_uploaded_frame + 1 ..= n_frames_in_wal]`
    /// and append them to `STATE.buffer`. Caches the WAL header on
    /// first call. Bumps the bookmark on success. No S3 traffic
    /// here  Stage 3 wraps this with a flush trigger.
    fn drain_frames(db_name: &str, n_frames_in_wal: u32) -> Result<(), String> {
        STATE.with(|s| -> Result<(), String> {
            let mut guard = s.borrow_mut();
            let state = match guard.as_mut() {
                Some(st) if st.started => st,
                _ => return Ok(()), // not started or stopped — silent no-op
            };
            // Lazy header fetch. The WAL header is 32 bytes
            // starting with the WAL magic (0x377F0682 LE or
            // 0x377F0683 BE). The page_size is at byte offset 8
            // as a big-endian u32. We cache the raw 32 bytes
            // verbatim so the restore path can reconstruct
            // `<target>-wal` byte-for-byte.
            if state.wal_header.is_none() {
                match wal_frames::get_wal_header(&db_name.to_string()) {
                    Ok(Some(bytes)) if bytes.len() == 32 => {
                        state.wal_header = Some(bytes);
                    }
                    // None: WAL file doesn't exist yet (no WAL
                    // commits since journal_mode=WAL took effect).
                    // We'll try again next firing. Same fall-through
                    // for unexpected sizes  bail without bumping
                    // the bookmark.
                    Ok(_) => return Ok(()),
                    Err(_) => return Ok(()),
                }
            }
            let next_frame = state.last_uploaded_frame.saturating_add(1);
            if next_frame > n_frames_in_wal as u64 {
                // No new frames to drain (race or duplicate
                // firing). Not an error.
                return Ok(());
            }
            let to_read = (n_frames_in_wal as u64 - next_frame + 1) as u32;
            match wal_frames::read_frames(
                &db_name.to_string(),
                next_frame as u32,
                to_read,
            ) {
                Ok(bytes) => {
                    state.buffer.extend_from_slice(&bytes);
                    state.last_uploaded_frame = n_frames_in_wal as u64;
                    Ok(())
                }
                // Retain the bookmark on a transient failure so
                // the next firing tries the same range again.
                Err(_) => Ok(()),
            }
        })
    }

    bindings::export!(Ext with_types_in bindings);
}
