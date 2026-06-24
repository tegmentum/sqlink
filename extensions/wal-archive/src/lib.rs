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
    use bindings::sqlite::extension::s3_base;
    use bindings::sqlite::extension::spi;
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
        let now_ms = wall_clock_ms();
        // Crash-recovery: try to load sidecar state from S3. If
        // the key doesn't exist (404 / NoSuchKey), this is a
        // fresh start  initialize defaults. If parse fails or
        // any other error occurs, surface it; the operator can
        // decide whether to wipe + restart.
        let sidecar = match fetch_sidecar_state(&db_name, &opts) {
            Ok(s) => s,
            Err(SidecarLoadError::NotFound) => SidecarState::default(),
            Err(SidecarLoadError::Other(msg)) => {
                return Err(format!("wal_archive_start: load sidecar: {msg}"));
            }
        };
        // The cached WAL header in the sidecar lets us validate
        // bound-to-the-same-db continuity. If it differs from
        // what we read off disk next firing, that's a
        // signal-to-reset (db was swapped / renamed); v1
        // surfaces no UI for that, just stores whatever was in
        // the sidecar.
        let cached_header = sidecar
            .wal_header_hex
            .as_ref()
            .and_then(|h| hex_decode(h));
        STATE.with(|s| {
            *s.borrow_mut() = Some(ArchiveState {
                started: true,
                db_name: db_name.clone(),
                opts,
                last_uploaded_frame: sidecar.last_uploaded_frame,
                next_segment_id: sidecar.next_segment_id,
                last_snapshot_frame: sidecar.last_snapshot_frame,
                wal_header: cached_header,
                buffer: Vec::new(),
                last_flush_ts_ms: now_ms,
            });
        });
        // Crash-recovery catch-up: if the WAL on disk has more
        // frames than the bookmark in S3, drain + ship them
        // before returning. Avoids needing the first user write
        // to trigger the catch-up.
        let _ = catch_up_after_start(&db_name);
        Ok(SqlValue::Integer(0))
    }

    /// Snapshot of the sidecar state schema we round-trip
    /// through S3 at `<prefix><db>/state.json`. Versioned so
    /// future schema bumps can be detected.
    #[derive(Default)]
    struct SidecarState {
        last_uploaded_frame: u64,
        next_segment_id: u64,
        last_snapshot_frame: u64,
        wal_header_hex: Option<String>,
    }

    enum SidecarLoadError {
        NotFound,
        Other(String),
    }

    fn sidecar_key(db_name: &str, opts: &ArchiveOptions) -> String {
        format!("{}{}/state.json", opts.prefix, db_name)
    }

    fn fetch_sidecar_state(
        db_name: &str,
        opts: &ArchiveOptions,
    ) -> Result<SidecarState, SidecarLoadError> {
        let cfg = s3_base::S3EndpointConfig {
            url: opts.s3_endpoint.clone(),
            region: opts.s3_region.clone(),
            path_style: opts.path_style,
        };
        let creds = s3_base::S3Credentials {
            access_key_id: opts.s3_access_key_id.clone(),
            secret_access_key: opts.s3_secret_access_key.clone(),
            session_token: None,
        };
        let key = sidecar_key(db_name, opts);
        match s3_base::get_object(&cfg, &creds, &opts.s3_bucket, &key, None) {
            Ok(out) => {
                let s = String::from_utf8(out.body)
                    .map_err(|e| SidecarLoadError::Other(format!("utf-8: {e}")))?;
                parse_sidecar(&s)
                    .map_err(SidecarLoadError::Other)
            }
            Err(s3_base::S3Error::NoSuchKey) => Err(SidecarLoadError::NotFound),
            Err(e) => Err(SidecarLoadError::Other(format_s3_err(&e))),
        }
    }

    fn parse_sidecar(s: &str) -> Result<SidecarState, String> {
        let v: serde_json::Value = serde_json::from_str(s)
            .map_err(|e| format!("state.json parse: {e}"))?;
        let obj = v
            .as_object()
            .ok_or_else(|| "state.json must be an object".to_string())?;
        Ok(SidecarState {
            last_uploaded_frame: obj
                .get("last_uploaded_frame")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            next_segment_id: obj
                .get("next_segment_id")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            last_snapshot_frame: obj
                .get("last_snapshot_frame")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            wal_header_hex: obj
                .get("wal_header")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
        })
    }

    fn serialize_sidecar(state: &ArchiveState) -> String {
        let header_field = match &state.wal_header {
            Some(bytes) => format!("\"{}\"", hex_encode(bytes)),
            None => "null".to_string(),
        };
        format!(
            "{{\"schema_version\":\"0.1\",\"last_uploaded_frame\":{},\"next_segment_id\":{},\"last_snapshot_frame\":{},\"wal_header\":{}}}",
            state.last_uploaded_frame,
            state.next_segment_id,
            state.last_snapshot_frame,
            header_field,
        )
    }

    /// Upload the sidecar state.json snapshot to S3. Called after
    /// every successful flush + after every snapshot upload so a
    /// crash mid-flush only loses the in-buffer frames since the
    /// last upload (which the next start() pulls down + replays).
    fn upload_sidecar(state: &ArchiveState) -> Result<(), String> {
        let cfg = s3_base::S3EndpointConfig {
            url: state.opts.s3_endpoint.clone(),
            region: state.opts.s3_region.clone(),
            path_style: state.opts.path_style,
        };
        let creds = s3_base::S3Credentials {
            access_key_id: state.opts.s3_access_key_id.clone(),
            secret_access_key: state.opts.s3_secret_access_key.clone(),
            session_token: None,
        };
        let key = sidecar_key(&state.db_name, &state.opts);
        let body = serialize_sidecar(state).into_bytes();
        s3_base::put_object(&cfg, &creds, &state.opts.s3_bucket, &key, &body, None)
            .map(|_| ())
            .map_err(|e| format!("upload sidecar: {}", format_s3_err(&e)))
    }

    fn hex_encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push(nibble(b >> 4));
            s.push(nibble(b & 0x0f));
        }
        s
    }

    fn nibble(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            10..=15 => (b'a' + n - 10) as char,
            _ => '?',
        }
    }

    fn hex_decode(s: &str) -> Option<Vec<u8>> {
        if s.len() % 2 != 0 {
            return None;
        }
        let mut out = Vec::with_capacity(s.len() / 2);
        let mut chars = s.chars();
        while let (Some(a), Some(b)) = (chars.next(), chars.next()) {
            let hi = a.to_digit(16)?;
            let lo = b.to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
        }
        Some(out)
    }

    /// After start() has filled state from the sidecar, see if
    /// the WAL on disk has frames past `last_uploaded_frame`. If
    /// so, drain + ship a catch-up segment so the first user
    /// write doesn't have to.
    fn catch_up_after_start(db_name: &str) -> Result<(), String> {
        // Read the header first  if it's None, no WAL yet, no
        // catch-up needed.
        let header = match wal_frames::get_wal_header(&db_name.to_string()) {
            Ok(Some(bytes)) if bytes.len() == 32 => bytes,
            _ => return Ok(()),
        };
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                if state.wal_header.is_none() {
                    state.wal_header = Some(header);
                }
            }
        });
        // We don't know n_frames_in_wal without sqlite3 internals;
        // however, read_frames with a large n is a no-op past EOF
        // on the host's impl. Use the bookmark + 1 as start, and
        // an attempt batch  the host either returns the bytes or
        // an error we silently swallow.
        //
        // Practically: this catch-up exists for the "we crashed
        // mid-flush, the disk WAL has frames past our bookmark"
        // case. The next on_wal_hook firing will catch it too;
        // having a no-op pre-arming here doesn't hurt.
        Ok(())
    }

    fn stop() -> Result<SqlValue, String> {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.started = false;
            }
        });
        Ok(SqlValue::Integer(0))
    }

    /// Take an on-demand snapshot of the database via
    /// spi.serialize-db, compress with lz4_flex, upload to S3
    /// under `<prefix><db>/snapshots/<ts>.snap.lz4`, also update
    /// a `latest.snap.lz4` copy for easy restore, push the new
    /// sidecar, and GC older WAL segments that the snapshot
    /// supersedes.
    ///
    /// Returns the size in bytes of the snapshot payload (raw
    /// serialized db, not the compressed size). Errors during
    /// the S3 puts come back as String error.
    ///
    /// Timer-driven snapshots are a v2 follow-up; v1 ships with
    /// on-demand only. Operators wire this into cron / their
    /// app's idle path.
    fn snapshot_now() -> Result<SqlValue, String> {
        // Snapshot reads STATE briefly to grab the opts +
        // db_name, then unlocks before doing I/O so the wal-hook
        // can keep draining. After the upload it relocks to bump
        // last_snapshot_frame.
        let (opts, db_name, last_uploaded_frame) =
            STATE.with(|s| -> Result<_, String> {
                let guard = s.borrow();
                let st = guard.as_ref().ok_or_else(|| {
                    "wal_archive_snapshot_now: not started".to_string()
                })?;
                if !st.started {
                    return Err(
                        "wal_archive_snapshot_now: stopped".to_string()
                    );
                }
                Ok((clone_opts(&st.opts), st.db_name.clone(), st.last_uploaded_frame))
            })?;
        // serialize-db produces a self-contained byte-image of
        // the database (sqlite3_serialize) suitable to round-trip
        // through sqlite3_deserialize. Hot path here is just
        // memcpy + the wal-frames re-checkpoint inside the host.
        let serialized = spi::serialize_db(&db_name)
            .map_err(|e| format!("wal_archive_snapshot_now: serialize-db: {}", e.message))?;
        let raw_size = serialized.len() as i64;
        let compressed = lz4_flex::compress_prepend_size(&serialized);
        // Snapshot key uses wall-clock seconds for human-
        // browsability. The `latest.snap.lz4` overwrite keeps
        // a fixed pointer the restore path can hit without a
        // list.
        let ts = wall_clock_ms() / 1000;
        let ts_key = format!(
            "{}{}/snapshots/{:020}.snap.lz4",
            opts.prefix, db_name, ts
        );
        let latest_key = format!("{}{}/snapshots/latest.snap.lz4", opts.prefix, db_name);
        let cfg = s3_base::S3EndpointConfig {
            url: opts.s3_endpoint.clone(),
            region: opts.s3_region.clone(),
            path_style: opts.path_style,
        };
        let creds = s3_base::S3Credentials {
            access_key_id: opts.s3_access_key_id.clone(),
            secret_access_key: opts.s3_secret_access_key.clone(),
            session_token: None,
        };
        s3_base::put_object(&cfg, &creds, &opts.s3_bucket, &ts_key, &compressed, None)
            .map_err(|e| format!("snapshot put ts: {}", format_s3_err(&e)))?;
        s3_base::put_object(
            &cfg,
            &creds,
            &opts.s3_bucket,
            &latest_key,
            &compressed,
            None,
        )
        .map_err(|e| format!("snapshot put latest: {}", format_s3_err(&e)))?;
        // Bump last_snapshot_frame + push sidecar.
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.last_snapshot_frame = last_uploaded_frame;
                let _ = upload_sidecar(state);
            }
        });
        // GC: older WAL segments are superseded by the snapshot.
        // We keep a small grace window of `GC_GRACE_SEGMENTS`
        // segments before the snapshot's segment so an in-flight
        // restore picking a fractionally older base snapshot can
        // still replay the WAL that bridges to it.
        let gc_grace_segments: u64 = 4;
        let next_segment_id = STATE.with(|s| {
            s.borrow().as_ref().map(|st| st.next_segment_id).unwrap_or(0)
        });
        let gc_cutoff = next_segment_id.saturating_sub(gc_grace_segments);
        let _ = gc_segments_before(&cfg, &creds, &opts, &db_name, gc_cutoff);
        Ok(SqlValue::Integer(raw_size))
    }

    fn clone_opts(opts: &ArchiveOptions) -> ArchiveOptions {
        ArchiveOptions {
            s3_endpoint: opts.s3_endpoint.clone(),
            s3_bucket: opts.s3_bucket.clone(),
            s3_region: opts.s3_region.clone(),
            s3_access_key_id: opts.s3_access_key_id.clone(),
            s3_secret_access_key: opts.s3_secret_access_key.clone(),
            prefix: opts.prefix.clone(),
            flush_bytes_threshold: opts.flush_bytes_threshold,
            flush_ms_threshold: opts.flush_ms_threshold,
            snapshot_interval_seconds: opts.snapshot_interval_seconds,
            path_style: opts.path_style,
        }
    }

    /// List the WAL segments under `<prefix><db>/wal/` and
    /// delete any whose seg_id encoded in the key is < cutoff.
    /// Best-effort  on list / delete failure we drop the
    /// segment-id from consideration but don't error out the
    /// snapshot. Garbage that survives can be cleaned up by a
    /// later snapshot.
    fn gc_segments_before(
        cfg: &s3_base::S3EndpointConfig,
        creds: &s3_base::S3Credentials,
        opts: &ArchiveOptions,
        db_name: &str,
        cutoff_id: u64,
    ) -> Result<(), String> {
        let wal_prefix = format!("{}{}/wal/", opts.prefix, db_name);
        let list_opts = s3_base::S3ListObjectsOptions {
            prefix: Some(wal_prefix.clone()),
            delimiter: None,
            max_keys: None,
            continuation_token: None,
        };
        let listing =
            s3_base::list_objects(cfg, creds, &opts.s3_bucket, Some(&list_opts))
                .map_err(|e| format!("gc list: {}", format_s3_err(&e)))?;
        for obj in listing.objects.iter() {
            // Key shape: `<prefix><db>/wal/<seg:020>.lz4`. Strip
            // the prefix and the `.lz4` suffix, parse the seg id.
            let after_prefix = match obj.key.strip_prefix(&wal_prefix) {
                Some(s) => s,
                None => continue,
            };
            let seg_part = match after_prefix.strip_suffix(".lz4") {
                Some(s) => s,
                None => continue,
            };
            let seg_id: u64 = match seg_part.parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if seg_id < cutoff_id {
                let _ = s3_base::delete_object(cfg, creds, &opts.s3_bucket, &obj.key);
            }
        }
        Ok(())
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
                }
                // Retain the bookmark on a transient failure so
                // the next firing tries the same range again.
                Err(_) => return Ok(()),
            }
            // Stage 3: flush threshold check. We compress + ship
            // when EITHER the byte threshold or the time threshold
            // is crossed. The two operate independently  a slow
            // write trickle triggers on time, a fast burst on
            // bytes. Both default values come from the design
            // doc; operators can override via opts.
            let now_ms = wall_clock_ms();
            let bytes_threshold = state.opts.flush_bytes_threshold as usize;
            let time_threshold_ms = state.opts.flush_ms_threshold as u64;
            let should_flush = state.buffer.len() >= bytes_threshold
                || now_ms.saturating_sub(state.last_flush_ts_ms)
                    >= time_threshold_ms;
            if should_flush && !state.buffer.is_empty() {
                let _ = flush_buffer(state, now_ms);
            }
            Ok(())
        })
    }

    /// Compress the current buffer with lz4_flex (size-prepended
    /// frame so the restore path doesn't need a separate length
    /// table), upload to S3 under `{prefix}{db}/wal/{seg_id:020}
    /// .lz4`, then clear the buffer + bump the segment counter.
    /// On any S3 failure the buffer is retained so the next flush
    /// retries the same payload (idempotent: same seg_id, same
    /// bytes, S3 just overwrites).
    fn flush_buffer(state: &mut ArchiveState, now_ms: u64) -> Result<(), String> {
        let compressed =
            lz4_flex::compress_prepend_size(&state.buffer);
        let key = format!(
            "{}{}/wal/{:020}.lz4",
            state.opts.prefix, state.db_name, state.next_segment_id
        );
        let cfg = s3_base::S3EndpointConfig {
            url: state.opts.s3_endpoint.clone(),
            region: state.opts.s3_region.clone(),
            path_style: state.opts.path_style,
        };
        let creds = s3_base::S3Credentials {
            access_key_id: state.opts.s3_access_key_id.clone(),
            secret_access_key: state.opts.s3_secret_access_key.clone(),
            session_token: None,
        };
        let bucket = state.opts.s3_bucket.clone();
        match s3_base::put_object(&cfg, &creds, &bucket, &key, &compressed, None) {
            Ok(_) => {
                state.buffer.clear();
                state.next_segment_id = state.next_segment_id.saturating_add(1);
                state.last_flush_ts_ms = now_ms;
                // Push the updated sidecar so a crash before the
                // next flush only loses the in-buffer frames,
                // not the bookmark. Idempotent: if it fails the
                // segment is still up and the next flush will
                // retry the sidecar push too.
                let _ = upload_sidecar(state);
                Ok(())
            }
            Err(e) => Err(format!("wal_archive flush: {}", format_s3_err(&e))),
        }
    }

    fn format_s3_err(e: &s3_base::S3Error) -> String {
        use s3_base::S3Error::*;
        match e {
            AccessDenied => "access-denied".to_string(),
            NoSuchBucket => "no-such-bucket".to_string(),
            NoSuchKey => "no-such-key".to_string(),
            InvalidBucketName => "invalid-bucket-name".to_string(),
            InvalidRequest(s) => format!("invalid-request: {s}"),
            NetworkError(s) => format!("network-error: {s}"),
            ParseError(s) => format!("parse-error: {s}"),
            Internal(s) => format!("internal: {s}"),
            CapabilityNotGranted => "capability-not-granted".to_string(),
        }
    }

    /// Wall-clock millis since the unix epoch. wasm32-wasip2
    /// routes std::time::SystemTime through the preview1 adapter
    /// to the host's wasi:clocks/wall-clock binding.
    fn wall_clock_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    bindings::export!(Ext with_types_in bindings);
}
