//! tar archive parsing as SQL scalars. Wraps the `tar` 0.4 crate
//! so callers can introspect a tar archive that lives in a BLOB
//! column (e.g. one row per archive, downloaded HTTP body, sqlar
//! payload).
//!
//! Surface (v1, scalars-only; vtab is a follow-up):
//!
//!   tar_entry_names(blob)        -> TEXT (JSON array of strings)
//!   tar_entry_count(blob)        -> INTEGER
//!   tar_entry_size(blob, name)   -> INTEGER (bytes in body)
//!   tar_entry_data(blob, name)   -> BLOB    (full entry body)
//!   tar_entry_mtime(blob, name)  -> INTEGER (epoch seconds)
//!   tar_entry_mode(blob, name)   -> INTEGER (unix permissions)
//!   tar_is_valid(blob)           -> INTEGER (1 = parses, 0 = no)
//!   tar_version()                -> TEXT
//!
//! Contract:
//!   * Bad blob or absent entry  NULL (never Err).
//!   * tar_is_valid is the one exception: it returns 0 on a bad
//!     blob, 1 on a parseable archive (with at least zero entries
//!     readable). Callers can branch on it before pulling entries.
//!   * Lookup matches the FIRST entry whose path equals `name`
//!     (tar permits duplicates; the first one wins, mirroring
//!     `tar -xf` extraction order).
//!
//! Why scalars-only first: the vtab path needs a row producer
//! that holds the parsed archive across xFilter/xColumn calls,
//! which is a separate state-management story (see zipfile vtab
//! for the template). Scalars cover "I have a blob, give me one
//! fact about it" which is the bulk of the use-case.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use std::io::{Cursor, Read};

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    // ---- Function IDs (stable; do not renumber). ----
    const FID_ENTRY_NAMES: u64 = 1;
    const FID_ENTRY_COUNT: u64 = 2;
    const FID_ENTRY_SIZE: u64 = 3;
    const FID_ENTRY_DATA: u64 = 4;
    const FID_ENTRY_MTIME: u64 = 5;
    const FID_ENTRY_MODE: u64 = 6;
    const FID_IS_VALID: u64 = 7;
    const FID_VERSION: u64 = 8;

    struct Ext;

    // ---- Arg helpers ----

    /// Pull a BLOB or TEXT arg as raw bytes. Returns None for
    /// NULL or any other type — callers map None to SQL NULL.
    fn opt_bytes(args: &[SqlValue], i: usize) -> Option<Vec<u8>> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// Pull a TEXT arg. Returns None for NULL or any other type.
    fn opt_text(args: &[SqlValue], i: usize) -> Option<String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            _ => None,
        }
    }

    // ---- Tar parsing helpers ----

    /// Iterate the archive and collect entry paths in declaration
    /// order. Returns None on parse failure.
    fn collect_names(bytes: &[u8]) -> Option<Vec<String>> {
        let mut a = tar::Archive::new(Cursor::new(bytes));
        let entries = a.entries().ok()?;
        let mut out = Vec::new();
        for e in entries {
            let e = e.ok()?;
            let path = e.path().ok()?;
            out.push(path.to_string_lossy().into_owned());
        }
        Some(out)
    }

    /// Find the first entry whose path matches `name` and hand
    /// it to a callback. Returns None on parse fail OR if no
    /// entry matches.
    fn with_entry<R, F>(bytes: &[u8], name: &str, mut f: F) -> Option<R>
    where
        F: FnMut(&mut tar::Entry<'_, Cursor<&[u8]>>) -> Option<R>,
    {
        let mut a = tar::Archive::new(Cursor::new(bytes));
        let entries = a.entries().ok()?;
        for e in entries {
            let mut e = e.ok()?;
            let p = e.path().ok()?;
            if p.to_string_lossy() == name {
                return f(&mut e);
            }
        }
        None
    }

    /// JSON-encode a list of strings into a SQL TEXT value.
    /// We hand-roll instead of pulling serde_json — keeps the
    /// wasm binary small and the dependency graph short.
    fn json_array(items: &[String]) -> String {
        let mut s = String::from("[");
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('"');
            for c in item.chars() {
                match c {
                    '"' => s.push_str("\\\""),
                    '\\' => s.push_str("\\\\"),
                    '\n' => s.push_str("\\n"),
                    '\r' => s.push_str("\\r"),
                    '\t' => s.push_str("\\t"),
                    c if (c as u32) < 0x20 => {
                        s.push_str(&format!("\\u{:04x}", c as u32));
                    }
                    c => s.push(c),
                }
            }
            s.push('"');
        }
        s.push(']');
        s
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "tar".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENTRY_NAMES, "tar_entry_names", 1, det),
                    s(FID_ENTRY_COUNT, "tar_entry_count", 1, det),
                    s(FID_ENTRY_SIZE, "tar_entry_size", 2, det),
                    s(FID_ENTRY_DATA, "tar_entry_data", 2, det),
                    s(FID_ENTRY_MTIME, "tar_entry_mtime", 2, det),
                    s(FID_ENTRY_MODE, "tar_entry_mode", 2, det),
                    s(FID_IS_VALID, "tar_is_valid", 1, det),
                    s(FID_VERSION, "tar_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_ENTRY_NAMES => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    match collect_names(&bytes) {
                        Some(names) => Ok(SqlValue::Text(json_array(&names))),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_ENTRY_COUNT => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    match collect_names(&bytes) {
                        Some(names) => Ok(SqlValue::Integer(names.len() as i64)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_ENTRY_SIZE => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(name) = opt_text(&args, 1) else {
                        return Ok(SqlValue::Null);
                    };
                    match with_entry(&bytes, &name, |e| Some(e.size() as i64)) {
                        Some(n) => Ok(SqlValue::Integer(n)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_ENTRY_DATA => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(name) = opt_text(&args, 1) else {
                        return Ok(SqlValue::Null);
                    };
                    let data = with_entry(&bytes, &name, |e| {
                        let mut buf = Vec::with_capacity(e.size() as usize);
                        e.read_to_end(&mut buf).ok()?;
                        Some(buf)
                    });
                    match data {
                        Some(b) => Ok(SqlValue::Blob(b)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_ENTRY_MTIME => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(name) = opt_text(&args, 1) else {
                        return Ok(SqlValue::Null);
                    };
                    let t = with_entry(&bytes, &name, |e| {
                        e.header().mtime().ok().map(|n| n as i64)
                    });
                    match t {
                        Some(n) => Ok(SqlValue::Integer(n)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_ENTRY_MODE => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(name) = opt_text(&args, 1) else {
                        return Ok(SqlValue::Null);
                    };
                    let m = with_entry(&bytes, &name, |e| {
                        e.header().mode().ok().map(|n| n as i64)
                    });
                    match m {
                        Some(n) => Ok(SqlValue::Integer(n)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_IS_VALID => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Integer(0));
                    };
                    let ok = collect_names(&bytes).is_some();
                    Ok(SqlValue::Integer(if ok { 1 } else { 0 }))
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("tar: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
