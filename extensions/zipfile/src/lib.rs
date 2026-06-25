//! zipfile vtab. Reads a zip archive by file path. xCreate
//! captures the path; xFilter opens the archive and
//! materialises each entry into the cursor on first read.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;
    use std::io::Read;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "tabular",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan,
    VtabRow};
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID: u64 = 1;

    #[derive(Clone)]
    struct Instance {
        path: String,
    }

    struct Entry {
        name: String,
        mode: u32,
        mtime: i64,
        sz: u64,
        data: Vec<u8>,
        method: String,
    }

    struct Cursor {
        instance_id: u64,
        entries: Vec<Entry>,
        idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> = RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    struct Ext;

    fn parse_args(args: &[String]) -> Result<Instance, String> {
        // First user arg is the path; allow either bare
        // 'path/to.zip' or path='path/to.zip'.
        if args.is_empty() {
            return Err("zipfile: archive path required".to_string());
        }
        let first = args[0].trim();
        let path = if let Some((k, v)) = first.split_once('=') {
            if k.trim() != "path" {
                return Err(format!("zipfile: unknown arg {k:?}"));
            }
            strip_quotes(v.trim()).to_string()
        } else {
            strip_quotes(first).to_string()
        };
        Ok(Instance { path })
    }

    fn strip_quotes(s: &str) -> &str {
        let s = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(s);
        s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
    }

    fn schema() -> String {
        "CREATE TABLE x(name TEXT, mode INTEGER, mtime INTEGER, sz INTEGER, data BLOB, method TEXT)"
            .to_string()
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "zipfile".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "zipfile".to_string(),
                    eponymous: false,
                    mutable: false,
                    batched: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_: u64, _: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("zipfile: no scalar functions exported".to_string())
        }
    }

    fn method_name(m: zip::CompressionMethod) -> &'static str {
        use zip::CompressionMethod::*;
        match m {
            Stored => "store",
            Deflated => "deflate",
            _ => "other",
        }
    }

    fn read_archive(path: &str) -> Result<Vec<Entry>, String> {
        let f = std::fs::File::open(path)
            .map_err(|e| format!("zipfile: open {path}: {e}"))?;
        let mut archive = zip::ZipArchive::new(f)
            .map_err(|e| format!("zipfile: read archive: {e}"))?;
        let mut entries = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("zipfile: entry {i}: {e}"))?;
            let name = entry.name().to_string();
            let mode = entry.unix_mode().unwrap_or(0);
            let mtime = entry
                .last_modified()
                .and_then(|t| {
                    // zip's DateTime is local-tz naive; we can
                    // approximate by treating the components as
                    // UTC. Not strictly correct  documented.
                    chrono_like_to_unix(t)
                })
                .unwrap_or(0);
            let sz = entry.size();
            let mut data = Vec::with_capacity(sz as usize);
            entry
                .read_to_end(&mut data)
                .map_err(|e| format!("zipfile: read entry {name}: {e}"))?;
            let method = method_name(entry.compression()).to_string();
            entries.push(Entry { name, mode, mtime, sz, data, method });
        }
        Ok(entries)
    }

    /// Convert zip's `DateTime` to a unix epoch second. We
    /// don't pull chrono in here; build the calendar value by
    /// hand. Accurate to-the-second for dates past 1970.
    fn chrono_like_to_unix(t: zip::DateTime) -> Option<i64> {
        // zip::DateTime exposes year/month/day/hour/minute/second
        // accessors. Combine via the standard formula. NaiveDateTime
        // semantics: treat the value as UTC.
        let y = t.year() as i64;
        let mo = t.month() as i64;
        let d = t.day() as i64;
        let h = t.hour() as i64;
        let mi = t.minute() as i64;
        let s = t.second() as i64;
        // Days since 1970-01-01 to start of year y.
        if y < 1970 {
            return None;
        }
        let mut days: i64 = 0;
        for year in 1970..y {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            days += if leap { 366 } else { 365 };
        }
        let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        let leap_this = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        for m in 1..mo {
            days += month_days[m as usize - 1];
            if m == 2 && leap_this {
                days += 1;
            }
        }
        days += d - 1;
        Some(days * 86400 + h * 3600 + mi * 60 + s)
    }

    impl VtabGuest for Ext {
        fn create(
            _: u64,
            instance_id: u64,
            _: String,
            _: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            let inst = parse_args(&args)?;
            INSTANCES.with(|m| m.borrow_mut().insert(instance_id, inst));
            Ok(schema())
        }
        fn connect(
            v: u64,
            id: u64,
            d: String,
            t: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            <Self as VtabGuest>::create(v, id, d, t, args)
        }
        fn destroy(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }
        fn disconnect(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }
        fn best_index(_: u64, _: u64, info: IndexInfo) -> Result<IndexPlan, String> {
            // No special constraints honored; everything's a full
            // table scan.
            Ok(IndexPlan {
                constraint_usage: info
                    .constraints
                    .iter()
                    .map(|_| ConstraintUsage { argv_index: 0, omit: false })
                    .collect(),
                idx_num: 0,
                idx_str: None,
                estimated_cost: 1_000_000.0,
                estimated_rows: 1_000,
                orderby_consumed: false,
            })
        }
        fn open(_: u64, instance_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor { instance_id, entries: Vec::new(), idx: 0 },
                )
            });
            Ok(())
        }
        fn close(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| m.borrow_mut().remove(&cursor_id));
            Ok(())
        }
        fn filter(
            _: u64,
            cursor_id: u64,
            _: i32,
            _: Option<String>,
            _: Vec<SqlValue>,
        ) -> Result<(), String> {
            let inst_id = CURSORS.with(|cm| {
                cm.borrow().get(&cursor_id).map(|c| c.instance_id).unwrap_or(0)
            });
            let inst = INSTANCES
                .with(|m| m.borrow().get(&inst_id).cloned())
                .ok_or_else(|| "zipfile: instance not connected".to_string())?;
            let entries = read_archive(&inst.path)?;
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.entries = entries;
                    c.idx = 0;
                }
            });
            Ok(())
        }
        fn next(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.idx += 1;
                }
            });
            Ok(())
        }
        fn eof(_: u64, cursor_id: u64) -> bool {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| c.idx >= c.entries.len())
                    .unwrap_or(true)
            })
        }
        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "zipfile: cursor not open".to_string())?;
                let entry = c
                    .entries
                    .get(c.idx)
                    .ok_or_else(|| "zipfile: row past EOF".to_string())?;
                match col {
                    0 => Ok(SqlValue::Text(entry.name.clone())),
                    1 => Ok(SqlValue::Integer(entry.mode as i64)),
                    2 => Ok(SqlValue::Integer(entry.mtime)),
                    3 => Ok(SqlValue::Integer(entry.sz as i64)),
                    4 => Ok(SqlValue::Blob(entry.data.clone())),
                    5 => Ok(SqlValue::Text(entry.method.clone())),
                    other => Err(format!("zipfile: bad column {other}")),
                }
            })
        }
        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "zipfile: cursor not open".to_string())
            })
        }
    
        fn fetch_batch(
            _vtab_id: u64,
            _cursor_id: u64,
            _max_rows: u32,
        ) -> Result<Vec<VtabRow>, String> {
            Err("fetch_batch: not implemented; host falls back to per-row".to_string())
        }
}

    bindings::export!(Ext with_types_in bindings);
}
