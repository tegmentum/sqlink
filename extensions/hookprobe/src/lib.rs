//! hookprobe  test-bench extension that wires every hook surface
//! to a single in-memory event log. The extension declares
//! has-authorizer + has-update-hook + has-commit-hook in its
//! manifest so the cli's `.load` walks the spi-loader's three
//! register-* calls, each of which lands a dispatch-bridge
//! trampoline on the shared connection.
//!
//! Exports two scalar functions for tests to drive + observe the
//! hook state:
//!
//!   hookprobe_drain_log() -> TEXT
//!       Returns a JSON array of the events recorded since the
//!       last drain (and clears the log). Each entry is a
//!       string of the form:
//!         "update:insert:main:t:1"
//!         "commit"
//!         "rollback"
//!         "authorize:read:main:t:id"
//!
//!   hookprobe_deny_table(name TEXT) -> NULL
//!       Tells the authorizer to deny every SQLITE_READ that
//!       targets `name` until a subsequent
//!       hookprobe_deny_table(NULL) clears it. Passing NULL
//!       clears the denylist.
//!
//!   hookprobe_veto_commit(1|0) -> NULL
//!       When 1, the next commit-hook call returns false
//!       (aborts the commit). Resets to 0 after firing.
//!
//! v1 limitation: state is per-instance (each extension load gets
//! its own log). That's fine for the browser test  the extension
//! is loaded once per page.

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
            world: "hookprobe",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::authorizer::Guest as AuthorizerGuest;
    use bindings::exports::sqlite::extension::commit_hook::Guest as CommitHookGuest;
    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::update_hook::Guest as UpdateHookGuest;
    use bindings::exports::sqlite::extension::wal_hook::Guest as WalHookGuest;
    use bindings::sqlite::extension::policy::Capability;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{
        AuthAction, AuthResult, FunctionFlags, SqlValue, UpdateOperation,
    };
    use bindings::sqlite::extension::s3_base;
    use bindings::sqlite::extension::wal_frames;

    // Scalar function ids.
    const FID_DRAIN: u64 = 1;
    const FID_DENY: u64 = 2;
    const FID_VETO: u64 = 3;
    /// hookprobe_wal_header() -> BLOB or NULL
    const FID_WAL_HEADER: u64 = 4;
    /// hookprobe_read_frames(start, n) -> BLOB
    const FID_READ_FRAMES: u64 = 5;
    /// hookprobe_serialize_main() -> BLOB
    const FID_SERIALIZE_MAIN: u64 = 6;
    /// hookprobe_s3_put(endpoint, region, access_key, secret_key,
    ///                  bucket, key, body) -> INTEGER (200/204 status)
    const FID_S3_PUT: u64 = 7;
    /// hookprobe_s3_get(endpoint, region, access_key, secret_key,
    ///                  bucket, key) -> BLOB
    const FID_S3_GET: u64 = 8;
    /// hookprobe_s3_list(endpoint, region, access_key, secret_key,
    ///                   bucket, prefix) -> TEXT (JSON array of keys)
    const FID_S3_LIST: u64 = 9;
    /// hookprobe_s3_delete(endpoint, region, access_key, secret_key,
    ///                     bucket, key) -> INTEGER (status)
    const FID_S3_DELETE: u64 = 10;

    struct Ext;

    thread_local! {
        /// Event log filled by the four hook callbacks; drained by
        /// hookprobe_drain_log().
        static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };

        /// Table name (lowercased) the authorizer should deny reads
        /// from. None  no denylist active.
        static DENY_TABLE: RefCell<Option<String>> = const { RefCell::new(None) };

        /// If true, the next commit-hook call returns false
        /// (instructing dispatch-bridge to abort the commit). Auto-
        /// resets to false after firing so subsequent commits work.
        static VETO_NEXT_COMMIT: RefCell<bool> = const { RefCell::new(false) };
    }

    fn push(event: String) {
        LOG.with(|l| l.borrow_mut().push(event));
    }

    fn auth_action_name(a: &AuthAction) -> &'static str {
        match a {
            AuthAction::CreateIndex => "create-index",
            AuthAction::CreateTable => "create-table",
            AuthAction::CreateTempIndex => "create-temp-index",
            AuthAction::CreateTempTable => "create-temp-table",
            AuthAction::CreateTempTrigger => "create-temp-trigger",
            AuthAction::CreateTempView => "create-temp-view",
            AuthAction::CreateTrigger => "create-trigger",
            AuthAction::CreateView => "create-view",
            AuthAction::Delete => "delete",
            AuthAction::DropIndex => "drop-index",
            AuthAction::DropTable => "drop-table",
            AuthAction::DropTempIndex => "drop-temp-index",
            AuthAction::DropTempTable => "drop-temp-table",
            AuthAction::DropTempTrigger => "drop-temp-trigger",
            AuthAction::DropTempView => "drop-temp-view",
            AuthAction::DropTrigger => "drop-trigger",
            AuthAction::DropView => "drop-view",
            AuthAction::Insert => "insert",
            AuthAction::Pragma => "pragma",
            AuthAction::Read => "read",
            AuthAction::Select => "select",
            AuthAction::Transaction => "transaction",
            AuthAction::Update => "update",
            AuthAction::Attach => "attach",
            AuthAction::Detach => "detach",
            AuthAction::AlterTable => "alter-table",
            AuthAction::Reindex => "reindex",
            AuthAction::Analyze => "analyze",
            AuthAction::CreateVtable => "create-vtable",
            AuthAction::DropVtable => "drop-vtable",
            AuthAction::Function => "function",
            AuthAction::Savepoint => "savepoint",
            AuthAction::Recursive => "recursive",
        }
    }

    fn update_op_name(o: &UpdateOperation) -> &'static str {
        match o {
            UpdateOperation::Insert => "insert",
            UpdateOperation::Update => "update",
            UpdateOperation::Delete => "delete",
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "hookprobe".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    ScalarFunctionSpec {
                        id: FID_DRAIN,
                        name: "hookprobe_drain_log".to_string(),
                        num_args: 0,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_DENY,
                        name: "hookprobe_deny_table".to_string(),
                        num_args: 1,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_VETO,
                        name: "hookprobe_veto_commit".to_string(),
                        num_args: 1,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_WAL_HEADER,
                        name: "hookprobe_wal_header".to_string(),
                        num_args: 0,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_READ_FRAMES,
                        name: "hookprobe_read_frames".to_string(),
                        num_args: 2,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_SERIALIZE_MAIN,
                        name: "hookprobe_serialize_main".to_string(),
                        num_args: 0,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_S3_PUT,
                        name: "hookprobe_s3_put".to_string(),
                        num_args: 7,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_S3_GET,
                        name: "hookprobe_s3_get".to_string(),
                        num_args: 6,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_S3_LIST,
                        name: "hookprobe_s3_list".to_string(),
                        num_args: 6,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                    ScalarFunctionSpec {
                        id: FID_S3_DELETE,
                        name: "hookprobe_s3_delete".to_string(),
                        num_args: 6,
                        func_flags: FunctionFlags::DIRECT_ONLY,
                    },
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: true,
                has_update_hook: true,
                has_commit_hook: true,
                has_wal_hook: true,
                wal_hook_id: WAL_HOOK_ID,
                dot_commands: alloc::vec![],
                // hookprobe_serialize_main calls spi.serialize-db
                // (capability::spi), hookprobe_wal_header /
                // hookprobe_read_frames call wal-frames.*
                // (capability::wal-frames), and the four
                // hookprobe_s3_* probes call s3-base.*
                // (capability::s3). The host's policy gate refuses
                // the load if any are missing from the operator's
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
                FID_DRAIN => {
                    let entries = LOG.with(|l| {
                        let mut v = l.borrow_mut();
                        let out: Vec<String> = v.drain(..).collect();
                        out
                    });
                    // Emit a minimal JSON array. Each entry is plain
                    // ASCII (no quotes / backslashes) by construction,
                    // so we can format without escaping.
                    let mut s = String::from("[");
                    for (i, e) in entries.iter().enumerate() {
                        if i > 0 {
                            s.push(',');
                        }
                        s.push('"');
                        s.push_str(e);
                        s.push('"');
                    }
                    s.push(']');
                    Ok(SqlValue::Text(s))
                }
                FID_DENY => {
                    let arg = args
                        .into_iter()
                        .next()
                        .ok_or_else(|| "hookprobe_deny_table: missing arg".to_string())?;
                    match arg {
                        SqlValue::Null => {
                            DENY_TABLE.with(|d| *d.borrow_mut() = None);
                        }
                        SqlValue::Text(t) => {
                            DENY_TABLE.with(|d| *d.borrow_mut() = Some(t.to_lowercase()));
                        }
                        _ => return Err("hookprobe_deny_table: arg must be TEXT or NULL".into()),
                    }
                    Ok(SqlValue::Null)
                }
                FID_VETO => {
                    let arg = args
                        .into_iter()
                        .next()
                        .ok_or_else(|| "hookprobe_veto_commit: missing arg".to_string())?;
                    let on = matches!(arg, SqlValue::Integer(n) if n != 0);
                    VETO_NEXT_COMMIT.with(|v| *v.borrow_mut() = on);
                    Ok(SqlValue::Null)
                }
                FID_WAL_HEADER => {
                    // wal-frames.get-wal-header("main"). The native
                    // host opens <db_path>-wal and returns the first
                    // 32 bytes; the composed-runtime host (sqlite-lib)
                    // returns None until vfs-tvm #437 lands.
                    match wal_frames::get_wal_header(&"main".to_string()) {
                        Ok(Some(bytes)) => Ok(SqlValue::Blob(bytes)),
                        Ok(None) => Ok(SqlValue::Null),
                        Err(e) => Err(format!(
                            "hookprobe_wal_header: get-wal-header: {}",
                            e.message
                        )),
                    }
                }
                FID_READ_FRAMES => {
                    let mut it = args.into_iter();
                    let start = match it.next() {
                        Some(SqlValue::Integer(n)) if n >= 1 => n as u32,
                        Some(SqlValue::Integer(n)) => {
                            return Err(format!(
                                "hookprobe_read_frames: start must be >= 1, got {n}"
                            ))
                        }
                        _ => return Err(
                            "hookprobe_read_frames: start arg must be INTEGER".into(),
                        ),
                    };
                    let n_frames = match it.next() {
                        Some(SqlValue::Integer(n)) if n >= 0 => n as u32,
                        Some(SqlValue::Integer(n)) => {
                            return Err(format!(
                                "hookprobe_read_frames: n must be >= 0, got {n}"
                            ))
                        }
                        _ => return Err(
                            "hookprobe_read_frames: n arg must be INTEGER".into(),
                        ),
                    };
                    match wal_frames::read_frames(&"main".to_string(), start, n_frames) {
                        Ok(bytes) => Ok(SqlValue::Blob(bytes)),
                        Err(e) => Err(format!(
                            "hookprobe_read_frames: read-frames: {}",
                            e.message
                        )),
                    }
                }
                FID_SERIALIZE_MAIN => {
                    // spi.serialize-db("main")  the existing one-shot
                    // serialize path the wal-archive snapshot cadence
                    // uses (no separate `backup` interface  see #439
                    // design call).
                    match spi::serialize_db(&"main".to_string()) {
                        Ok(bytes) => Ok(SqlValue::Blob(bytes)),
                        Err(e) => Err(format!(
                            "hookprobe_serialize_main: serialize-db: {}",
                            e.message
                        )),
                    }
                }
                FID_S3_PUT => {
                    // hookprobe_s3_put(endpoint, region, access_key,
                    //                  secret_key, bucket, key, body)
                    // Returns 200 on success.
                    let mut it = args.into_iter();
                    let endpoint_url = pop_text(&mut it, "endpoint")?;
                    let region = pop_text(&mut it, "region")?;
                    let ak = pop_text(&mut it, "access_key")?;
                    let sk = pop_text(&mut it, "secret_key")?;
                    let bucket = pop_text(&mut it, "bucket")?;
                    let key = pop_text(&mut it, "key")?;
                    let body = match it.next() {
                        Some(SqlValue::Blob(b)) => b,
                        Some(SqlValue::Text(t)) => t.into_bytes(),
                        _ => return Err("hookprobe_s3_put: body must be BLOB/TEXT".into()),
                    };
                    let cfg = s3_base::S3EndpointConfig {
                        url: endpoint_url,
                        region,
                        path_style: true,
                    };
                    let creds = s3_base::S3Credentials {
                        access_key_id: ak,
                        secret_access_key: sk,
                        session_token: None,
                    };
                    match s3_base::put_object(&cfg, &creds, &bucket, &key, &body, None) {
                        Ok(_) => Ok(SqlValue::Integer(200)),
                        Err(e) => Err(format!(
                            "hookprobe_s3_put: {}",
                            format_s3_err(&e)
                        )),
                    }
                }
                FID_S3_GET => {
                    let mut it = args.into_iter();
                    let endpoint_url = pop_text(&mut it, "endpoint")?;
                    let region = pop_text(&mut it, "region")?;
                    let ak = pop_text(&mut it, "access_key")?;
                    let sk = pop_text(&mut it, "secret_key")?;
                    let bucket = pop_text(&mut it, "bucket")?;
                    let key = pop_text(&mut it, "key")?;
                    let cfg = s3_base::S3EndpointConfig {
                        url: endpoint_url,
                        region,
                        path_style: true,
                    };
                    let creds = s3_base::S3Credentials {
                        access_key_id: ak,
                        secret_access_key: sk,
                        session_token: None,
                    };
                    match s3_base::get_object(&cfg, &creds, &bucket, &key, None) {
                        Ok(out) => Ok(SqlValue::Blob(out.body)),
                        Err(e) => Err(format!(
                            "hookprobe_s3_get: {}",
                            format_s3_err(&e)
                        )),
                    }
                }
                FID_S3_LIST => {
                    let mut it = args.into_iter();
                    let endpoint_url = pop_text(&mut it, "endpoint")?;
                    let region = pop_text(&mut it, "region")?;
                    let ak = pop_text(&mut it, "access_key")?;
                    let sk = pop_text(&mut it, "secret_key")?;
                    let bucket = pop_text(&mut it, "bucket")?;
                    let prefix = match it.next() {
                        Some(SqlValue::Text(s)) => Some(s),
                        Some(SqlValue::Null) => None,
                        _ => return Err("hookprobe_s3_list: prefix must be TEXT/NULL".into()),
                    };
                    let cfg = s3_base::S3EndpointConfig {
                        url: endpoint_url,
                        region,
                        path_style: true,
                    };
                    let creds = s3_base::S3Credentials {
                        access_key_id: ak,
                        secret_access_key: sk,
                        session_token: None,
                    };
                    let opts = s3_base::S3ListObjectsOptions {
                        prefix,
                        delimiter: None,
                        max_keys: None,
                        continuation_token: None,
                    };
                    match s3_base::list_objects(&cfg, &creds, &bucket, Some(&opts)) {
                        Ok(out) => {
                            let mut s = String::from("[");
                            for (i, obj) in out.objects.iter().enumerate() {
                                if i > 0 {
                                    s.push(',');
                                }
                                s.push('"');
                                // Naive escape: assume keys are safe
                                // ASCII (the test fixtures use plain
                                // `foo`-style keys). Anything else
                                // would need a proper JSON encoder.
                                s.push_str(&obj.key);
                                s.push('"');
                            }
                            s.push(']');
                            Ok(SqlValue::Text(s))
                        }
                        Err(e) => Err(format!(
                            "hookprobe_s3_list: {}",
                            format_s3_err(&e)
                        )),
                    }
                }
                FID_S3_DELETE => {
                    let mut it = args.into_iter();
                    let endpoint_url = pop_text(&mut it, "endpoint")?;
                    let region = pop_text(&mut it, "region")?;
                    let ak = pop_text(&mut it, "access_key")?;
                    let sk = pop_text(&mut it, "secret_key")?;
                    let bucket = pop_text(&mut it, "bucket")?;
                    let key = pop_text(&mut it, "key")?;
                    let cfg = s3_base::S3EndpointConfig {
                        url: endpoint_url,
                        region,
                        path_style: true,
                    };
                    let creds = s3_base::S3Credentials {
                        access_key_id: ak,
                        secret_access_key: sk,
                        session_token: None,
                    };
                    match s3_base::delete_object(&cfg, &creds, &bucket, &key) {
                        Ok(()) => Ok(SqlValue::Integer(204)),
                        Err(e) => Err(format!(
                            "hookprobe_s3_delete: {}",
                            format_s3_err(&e)
                        )),
                    }
                }
                _ => Err(format!("hookprobe: unknown func_id={func_id}")),
            }
        }
    }

    /// Pop the next arg as a non-null TEXT value or error out.
    fn pop_text(
        it: &mut alloc::vec::IntoIter<SqlValue>,
        field: &str,
    ) -> Result<String, String> {
        match it.next() {
            Some(SqlValue::Text(s)) => Ok(s),
            Some(other) => Err(format!(
                "hookprobe: arg {field} must be TEXT, got {other:?}"
            )),
            None => Err(format!("hookprobe: missing arg {field}")),
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

    impl AuthorizerGuest for Ext {
        fn authorize(
            action: AuthAction,
            arg1: Option<String>,
            _arg2: Option<String>,
            database: Option<String>,
            _trigger: Option<String>,
        ) -> AuthResult {
            // Record everything for the spec to assert on.
            push(format!(
                "authorize:{}:{}:{}:{}",
                auth_action_name(&action),
                database.as_deref().unwrap_or(""),
                arg1.as_deref().unwrap_or(""),
                _arg2.as_deref().unwrap_or(""),
            ));
            // Deny SELECTs that touch the deny-listed table.
            // SQLITE_READ's arg1 is the table name (per sqlite docs).
            if let (AuthAction::Read, Some(table)) = (&action, &arg1) {
                let denied = DENY_TABLE.with(|d| {
                    d.borrow()
                        .as_ref()
                        .is_some_and(|t| t.eq_ignore_ascii_case(table))
                });
                if denied {
                    return AuthResult::Deny;
                }
            }
            AuthResult::Ok
        }
    }

    impl UpdateHookGuest for Ext {
        fn on_update(operation: UpdateOperation, database: String, table: String, rowid: i64) {
            push(format!(
                "update:{}:{}:{}:{}",
                update_op_name(&operation),
                database,
                table,
                rowid
            ));
        }
    }

    impl CommitHookGuest for Ext {
        fn on_commit() -> bool {
            let allow = VETO_NEXT_COMMIT.with(|v| {
                let cur = *v.borrow();
                *v.borrow_mut() = false; // auto-reset
                !cur
            });
            push(format!("commit:{}", if allow { "allow" } else { "abort" }));
            allow
        }

        fn on_rollback() {
            push("rollback".to_string());
        }
    }

    /// WAL hook id the host echoes back to `wal-hook.on-wal-hook`.
    /// Declared in the manifest's `wal-hook-id` field; the native
    /// cli wires it via spi-loader.register-wal-hook(ext_name, 42)
    /// when it sees `has-wal-hook: true`. The browser spec
    /// explicitly calls `db.registerWalHook(ext_name, 42)` with the
    /// same constant so both deployment paths drive the same id
    /// through `on_wal_hook`.
    const WAL_HOOK_ID: u64 = 42;

    impl WalHookGuest for Ext {
        fn on_wal_hook(hook_id: u64, db_name: String, n_frames_in_wal: u32) -> i32 {
            // Record the event for the spec to assert on. n_frames is
            // SQLite's `nFrames` argument  the number of frames just
            // appended to the WAL for `db_name`.
            push(format!("wal:{}:{}:{}", hook_id, db_name, n_frames_in_wal));
            // SQLITE_OK  let the calling SQL statement proceed.
            0
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
