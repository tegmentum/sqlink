//! Dot-command dispatcher.
//!
//! Each `cmd_*` function takes the argument string (everything
//! after the command name) and returns the formatted output the
//! cli's `eval` should emit.
//!
//! Phase 2.2: state-only commands (.echo, .bail, .timer, .mode, etc.)
//! moved to the core-dotcmd registry. Their `cmd_*` helpers here are
//! kept for now as reference  the dispatcher no longer routes to
//! them, so dead-code warnings are silenced module-wide.

#![allow(dead_code)]

use std::cell::RefCell;
use std::os::raw::c_int;

use libsqlite3_sys as ffi;

use crate::db::{Connection, Value};
use crate::settings;

/// Run a parameterized query whose results are a single column;
/// collect column 0 from every row, stringifying via SQLite's
/// implicit coercion rules. Returns the cli "Error: ..." string
/// for prepare/bind/step failures.
fn query_text_col(conn: &Connection, sql: &str, params: &[Value]) -> Result<Vec<String>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| format!("Error: {}\n", e.message))?;
    stmt.bind_all(params)
        .map_err(|e| format!("Error: {}\n", e.message))?;
    let rows = stmt
        .collect_rows()
        .map_err(|e| format!("Error: {}\n", e.message))?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.into_iter().next().map(|v| match v {
            Value::Null => String::new(),
            Value::Integer(i) => i.to_string(),
            Value::Real(r) => r.to_string(),
            Value::Text(s) => s,
            Value::Blob(b) => format!("<blob:{} bytes>", b.len()),
        }))
        .collect())
}

/// Try to interpret `input` (already trimmed) as a dot-command.
/// Returns Some(output) if it was; None if not (caller falls back
/// to SQL execution).
pub fn dispatch(input: &str, conn: &Connection) -> Option<String> {
    let trimmed = input.trim();
    if !trimmed.starts_with('.') {
        return None;
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    Some(match cmd {
        // .help  routed through core-dotcmd registry.
        ".show" => cmd_show(),
        // .tables / .schema  routed through core-dotcmd registry.
        // .indexes  routed through the registry (core-dotcmd).
        // .databases  routed through core-dotcmd registry.
        // .headers / .mode / .nullvalue / .separator / .echo /
        // .prompt / .bail  routed through core-dotcmd registry
        // (Phase 2.2 cli-state writeback).
        // .print  routed through core-dotcmd registry.
        // .version is now provided by the `core-dotcmd` extension
        // (extensions/core-dotcmd). Phase 2 of PLAN-dotcmd-plugins.md
        // ports built-ins to the registry one at a time. When the
        // user loads core-dotcmd (or auto-load lands in Phase 2.5),
        // `.version` resolves through the loader fallthrough in
        // lib.rs. Without it loaded the user sees "Unknown command:
        // .version"  preferred over silently shadowing the new
        // registry surface.
        ".width" => cmd_width(arg),
        // .changes / .timer / .explain / .eqp / .stats  routed
        // through the core-dotcmd registry (Phase 2.2).
        ".timeout" => cmd_timeout(arg, conn),
        ".parameter" => cmd_parameter(arg),
        // .fullschema  routed through core-dotcmd registry.
        // .dbinfo  routed through the registry (core-dotcmd).
        ".dbconfig" => cmd_dbconfig(arg, conn),
        ".limit" => cmd_limit(arg, conn),
        // .binary  routed through core-dotcmd registry.
        // .log handled in lib.rs (the callback lives there, and
        // it depends on the install-time wiring done before
        // sqlite3 initialized).
        // .lint  routed through core-dotcmd registry.
        ".sha3sum" => cmd_sha3sum(arg, conn),
        ".sqlink" => cmd_sqlink(arg, conn),
        ".vfslist" => cmd_vfslist(),
        ".vfsname" => cmd_vfsname(arg, conn),
        ".archive" => cmd_archive(arg, conn),
        ".session" => cmd_session(arg, conn),
        ".serialize" => cmd_serialize(arg, conn),
        ".deserialize" => cmd_deserialize(arg, conn),
        _ => return None,
    })
}

/// `.serialize FILE` and `.deserialize FILE`  the sqlite3_serialize
/// / sqlite3_deserialize API surfaced as dot commands. Useful for
/// fixture-driven testing: capture a db state as a blob, distribute
/// it via fs, load it back into an in-memory db elsewhere.
///
/// Upstream sqlite3 cli exposes this via `.open --deserialize FILE`.
/// Our cli has a thinner `.open` (single-arg path); we add explicit
/// dot commands for clarity.

fn cmd_serialize(arg: &str, conn: &Connection) -> String {
    let file = arg.trim();
    if file.is_empty() {
        return "Usage: .serialize FILE\n".to_string();
    }
    let schema = match std::ffi::CString::new("main") {
        Ok(c) => c,
        Err(_) => return "Error: invalid schema name\n".to_string(),
    };
    let mut size: i64 = 0;
    // mFlags=0 means "give me a copy I own"  the returned buffer is
    // a fresh malloc that the caller frees via sqlite3_free.
    let ptr = unsafe {
        ds_ffi::sqlite3_serialize(conn.raw_handle(), schema.as_ptr(), &mut size, 0)
    };
    if ptr.is_null() {
        return "Error: sqlite3_serialize returned NULL (db has no main schema?)\n".to_string();
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(ptr, size as usize)
    }.to_vec();
    unsafe { ffi::sqlite3_free(ptr as *mut std::os::raw::c_void) };
    match std::fs::write(file, &bytes) {
        Ok(_) => format!("wrote {} bytes to {file}\n", bytes.len()),
        Err(e) => format!("Error: write {file}: {e}\n"),
    }
}

fn cmd_deserialize(arg: &str, conn: &Connection) -> String {
    let file = arg.trim();
    if file.is_empty() {
        return "Usage: .deserialize FILE\n".to_string();
    }
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => return format!("Error: read {file}: {e}\n"),
    };
    let schema = match std::ffi::CString::new("main") {
        Ok(c) => c,
        Err(_) => return "Error: invalid schema name\n".to_string(),
    };
    // sqlite3_deserialize takes ownership of the buffer when the
    // FREEONCLOSE flag is set. We allocate via sqlite3_malloc so
    // sqlite3 can free it; copy our bytes in; hand the pointer over.
    let size = bytes.len() as i64;
    let alloc = unsafe { ffi::sqlite3_malloc64(size as u64) };
    if alloc.is_null() {
        return "Error: sqlite3_malloc64 returned NULL\n".to_string();
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), alloc as *mut u8, bytes.len());
    }
    let rc = unsafe {
        ds_ffi::sqlite3_deserialize(
            conn.raw_handle(),
            schema.as_ptr(),
            alloc as *mut std::os::raw::c_uchar,
            size,
            size,
            ds_ffi::SQLITE_DESERIALIZE_FREEONCLOSE | ds_ffi::SQLITE_DESERIALIZE_RESIZEABLE,
        )
    };
    if rc != ffi::SQLITE_OK {
        unsafe { ffi::sqlite3_free(alloc) };
        return format!("Error: sqlite3_deserialize returned {rc}\n");
    }
    format!("loaded {} bytes from {file} into main\n", bytes.len())
}

// ----- .session (sqlite3 cli compatibility) ---------------------
//
// Matches the surface of the upstream sqlite3 shell's `.session`
// command. Sessions are per-name, scoped to this cli's connection,
// and tracked in a thread_local registry. Each session is a
// *mut sqlite3_session  the FFI handle that SQLITE_ENABLE_SESSION
// gives us.
//
// Subcommands:
//   .session NAME create [DB]      open a new session on DB (default "main")
//   .session NAME attach TABLE     attach a table (or "*" for all)
//   .session NAME enable on|off    toggle change recording
//   .session NAME indirect on|off  toggle indirect-changes flag
//   .session NAME isempty          print 1 or 0
//   .session NAME changeset FILE   write captured changeset to FILE
//   .session NAME patchset FILE    write captured patchset to FILE
//   .session NAME delete           close session (frees the handle)
//   .session list                  list active session names

/// Manual extern decls for sqlite3_serialize / sqlite3_deserialize.
/// Same rationale as session_ffi  the bundled sqlite3 has the
/// symbols via LIBSQLITE3_FLAGS, but the libsqlite3-sys feature
/// that would auto-declare them forces buildtime_bindgen.
mod ds_ffi {
    use std::os::raw::{c_char, c_int, c_uchar, c_uint};

    extern "C" {
        pub fn sqlite3_serialize(
            db: *mut libsqlite3_sys::sqlite3,
            zSchema: *const c_char,
            piSize: *mut i64,
            mFlags: c_uint,
        ) -> *mut c_uchar;

        pub fn sqlite3_deserialize(
            db: *mut libsqlite3_sys::sqlite3,
            zSchema: *const c_char,
            pData: *mut c_uchar,
            szDb: i64,
            szBuf: i64,
            mFlags: c_uint,
        ) -> c_int;
    }

    /// Free the buffer with sqlite3_free; allow sqlite to resize.
    pub const SQLITE_DESERIALIZE_FREEONCLOSE: c_uint = 1;
    pub const SQLITE_DESERIALIZE_RESIZEABLE: c_uint = 2;
}

/// Manual extern decls for sqlite3session_*. The bundled sqlite3
/// is compiled with SESSION + PREUPDATE_HOOK via LIBSQLITE3_FLAGS
/// in .cargo/config.toml, so the symbols are linkable; libsqlite3-sys's
/// `session` feature would auto-declare them but requires
/// buildtime_bindgen which fails to cross-compile to wasm32-wasip2.
mod session_ffi {
    use std::os::raw::{c_char, c_int, c_void};

    pub enum sqlite3_session {}

    extern "C" {
        pub fn sqlite3session_create(
            db: *mut libsqlite3_sys::sqlite3,
            zDb: *const c_char,
            ppSession: *mut *mut sqlite3_session,
        ) -> c_int;

        pub fn sqlite3session_delete(p: *mut sqlite3_session);

        pub fn sqlite3session_attach(p: *mut sqlite3_session, zTab: *const c_char) -> c_int;

        pub fn sqlite3session_enable(p: *mut sqlite3_session, bEnable: c_int) -> c_int;

        pub fn sqlite3session_indirect(p: *mut sqlite3_session, bIndirect: c_int) -> c_int;

        pub fn sqlite3session_isempty(p: *mut sqlite3_session) -> c_int;

        pub fn sqlite3session_changeset(
            p: *mut sqlite3_session,
            pnChangeset: *mut c_int,
            ppChangeset: *mut *mut c_void,
        ) -> c_int;

        pub fn sqlite3session_patchset(
            p: *mut sqlite3_session,
            pnPatch: *mut c_int,
            ppPatch: *mut *mut c_void,
        ) -> c_int;
    }
}

thread_local! {
    static SESSIONS: RefCell<std::collections::HashMap<String, *mut session_ffi::sqlite3_session>>
        = RefCell::new(std::collections::HashMap::new());
}

fn cmd_session(arg: &str, conn: &Connection) -> String {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return ".session NAME {create|attach|enable|indirect|isempty|changeset|patchset|delete}\n\
                .session list\n".to_string();
    }
    // First token is either "list" or a NAME.
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    if first == "list" {
        return session_list();
    }
    // first = NAME
    let name = first;
    let mut subparts = rest.splitn(2, char::is_whitespace);
    let sub = subparts.next().unwrap_or("").trim();
    let subarg = subparts.next().unwrap_or("").trim();
    match sub {
        "create" => session_create(name, subarg, conn),
        "attach" => session_attach(name, subarg),
        "enable" => session_enable(name, subarg),
        "indirect" => session_indirect(name, subarg),
        "isempty" => session_isempty(name),
        "changeset" => session_changeset(name, subarg, false),
        "patchset" => session_changeset(name, subarg, true),
        "delete" => session_delete(name),
        other => format!(".session {name}: unknown subcommand {other:?}\n\
                          Valid: create attach enable indirect isempty \
                          changeset patchset delete\n"),
    }
}

fn session_list() -> String {
    SESSIONS.with(|m| {
        let map = m.borrow();
        if map.is_empty() {
            return "(no active sessions)\n".to_string();
        }
        let mut names: Vec<&String> = map.keys().collect();
        names.sort();
        let mut out = String::new();
        for n in names {
            out.push_str(n);
            out.push('\n');
        }
        out
    })
}

fn session_create(name: &str, db_arg: &str, conn: &Connection) -> String {
    if SESSIONS.with(|m| m.borrow().contains_key(name)) {
        return format!("Error: session {name:?} already exists\n");
    }
    let db_name = if db_arg.is_empty() { "main" } else { db_arg };
    let db_c = match std::ffi::CString::new(db_name) {
        Ok(c) => c,
        Err(_) => return format!("Error: db name {db_name:?} has interior NUL\n"),
    };
    let mut session: *mut session_ffi::sqlite3_session = std::ptr::null_mut();
    let rc = unsafe {
        session_ffi::sqlite3session_create(conn.raw_handle(), db_c.as_ptr(), &mut session)
    };
    if rc != ffi::SQLITE_OK {
        return format!("Error: sqlite3session_create returned {rc}\n");
    }
    SESSIONS.with(|m| m.borrow_mut().insert(name.to_string(), session));
    String::new()
}

fn with_session<F: FnOnce(*mut session_ffi::sqlite3_session) -> String>(name: &str, f: F) -> String {
    let session = SESSIONS.with(|m| m.borrow().get(name).copied());
    match session {
        Some(s) => f(s),
        None => format!("Error: no session named {name:?}\n"),
    }
}

fn session_attach(name: &str, tbl: &str) -> String {
    with_session(name, |session| {
        let tbl_c = if tbl.is_empty() || tbl == "*" {
            None
        } else {
            match std::ffi::CString::new(tbl) {
                Ok(c) => Some(c),
                Err(_) => return format!("Error: table {tbl:?} has interior NUL\n"),
            }
        };
        let tbl_ptr = tbl_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
        let rc = unsafe { session_ffi::sqlite3session_attach(session, tbl_ptr) };
        if rc != ffi::SQLITE_OK {
            format!("Error: sqlite3session_attach returned {rc}\n")
        } else {
            String::new()
        }
    })
}

fn parse_on_off_strict(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Some(true),
        "off" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn session_enable(name: &str, arg: &str) -> String {
    with_session(name, |session| {
        let want: c_int = match parse_on_off_strict(arg) {
            Some(true) => 1,
            Some(false) => 0,
            None => return format!("Error: .session enable expects on|off (got {arg:?})\n"),
        };
        let _prev = unsafe { session_ffi::sqlite3session_enable(session, want) };
        String::new()
    })
}

fn session_indirect(name: &str, arg: &str) -> String {
    with_session(name, |session| {
        let want: c_int = match parse_on_off_strict(arg) {
            Some(true) => 1,
            Some(false) => 0,
            None => return format!("Error: .session indirect expects on|off (got {arg:?})\n"),
        };
        let _prev = unsafe { session_ffi::sqlite3session_indirect(session, want) };
        String::new()
    })
}

fn session_isempty(name: &str) -> String {
    with_session(name, |session| {
        let v = unsafe { session_ffi::sqlite3session_isempty(session) };
        format!("{}\n", if v != 0 { 1 } else { 0 })
    })
}

fn session_changeset(name: &str, file: &str, patchset: bool) -> String {
    if file.is_empty() {
        let which = if patchset { "patchset" } else { "changeset" };
        return format!("Error: .session {which} expects FILE\n");
    }
    with_session(name, |session| {
        let mut out_n: c_int = 0;
        let mut out_p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = if patchset {
            unsafe { session_ffi::sqlite3session_patchset(session, &mut out_n, &mut out_p) }
        } else {
            unsafe { session_ffi::sqlite3session_changeset(session, &mut out_n, &mut out_p) }
        };
        if rc != ffi::SQLITE_OK {
            return format!("Error: extract returned {rc}\n");
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(out_p as *const u8, out_n as usize)
        }.to_vec();
        unsafe { ffi::sqlite3_free(out_p) };
        match std::fs::write(file, &bytes) {
            Ok(_) => format!("wrote {} bytes to {file}\n", bytes.len()),
            Err(e) => format!("Error: write {file}: {e}\n"),
        }
    })
}

fn session_delete(name: &str) -> String {
    let session = SESSIONS.with(|m| m.borrow_mut().remove(name));
    match session {
        Some(s) => {
            unsafe { session_ffi::sqlite3session_delete(s) };
            String::new()
        }
        None => format!("Error: no session named {name:?}\n"),
    }
}

fn cmd_lint(arg: &str, conn: &Connection) -> String {
    match arg.trim() {
        "" | "fkey-indexes" => lint_fkey_indexes(conn),
        other => format!("Usage: .lint fkey-indexes (got {other:?})\n"),
    }
}

/// Reports every foreign key that has no usable index backing it.
/// Mirrors sqlite3's own `.lint fkey-indexes` output: one
/// suggested CREATE INDEX per missing index.
fn lint_fkey_indexes(conn: &Connection) -> String {
    // Walk every user table and its declared foreign keys. For each
    // FK, look for any existing index whose first column matches
    // the FK's `from` column. If none exists, emit a CREATE INDEX
    // suggestion. Doesn't catch the general "composite index covers
    // composite FK" case but matches the common pattern.
    let tables = match query_text_col(
        conn,
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        &[],
    ) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let mut out = String::new();
    for tbl in tables {
        let fk_sql = format!(
            "SELECT \"from\", \"table\", \"to\" FROM pragma_foreign_key_list('{}')",
            tbl.replace('\'', "''")
        );
        let mut stmt = match conn.prepare(&fk_sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rows = match stmt.collect_rows() {
            Ok(r) => r,
            Err(_) => continue,
        };
        drop(stmt);
        for row in rows {
            let from_col = match row.first() {
                Some(Value::Text(s)) => s.clone(),
                _ => continue,
            };
            // Does the table already have an index whose first
            // column is `from_col`? Inspect every index_list entry
            // and ask index_info(0).
            let idx_sql = format!(
                "SELECT name FROM pragma_index_list('{}')",
                tbl.replace('\'', "''")
            );
            let mut found = false;
            if let Ok(mut s) = conn.prepare(&idx_sql) {
                if let Ok(idxs) = s.collect_rows() {
                    for idx in idxs {
                        let idx_name = match idx.first() {
                            Some(Value::Text(s)) => s.clone(),
                            _ => continue,
                        };
                        let info_sql = format!(
                            "SELECT name FROM pragma_index_info('{}') ORDER BY seqno LIMIT 1",
                            idx_name.replace('\'', "''")
                        );
                        if let Ok(cols) = query_text_col(conn, &info_sql, &[]) {
                            if cols.first().map(|s| s.as_str()) == Some(from_col.as_str()) {
                                found = true;
                                break;
                            }
                        }
                    }
                }
            }
            if !found {
                out.push_str(&format!(
                    "CREATE INDEX 'idx_{tbl}_{from_col}' ON \"{tbl}\"(\"{from_col}\"); -- backs FK\n",
                    tbl = tbl, from_col = from_col
                ));
            }
        }
    }
    if out.is_empty() {
        "(no missing FK indexes)\n".to_string()
    } else {
        out
    }
}

fn cmd_sha3sum(arg: &str, conn: &Connection) -> String {
    use sha3::{Digest, Sha3_256};
    let table_filter = arg.trim();
    let where_clause = if table_filter.is_empty() || table_filter == "*" {
        "type='table' AND name NOT LIKE 'sqlite_%'".to_string()
    } else {
        let esc = table_filter.replace('\'', "''");
        format!("type='table' AND name='{esc}'")
    };
    let sql = format!(
        "SELECT name FROM sqlite_master WHERE {where_clause} ORDER BY name"
    );
    let tables = match query_text_col(conn, &sql, &[]) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if tables.is_empty() {
        return "(no tables matched)\n".to_string();
    }
    let mut out = String::new();
    for tbl in tables {
        let mut hasher = Sha3_256::new();
        let select = format!("SELECT * FROM \"{}\"", tbl.replace('"', "\"\""));
        let mut s = match conn.prepare(&select) {
            Ok(s) => s,
            Err(e) => {
                out.push_str(&format!("Error reading {tbl}: {}\n", e.message));
                continue;
            }
        };
        let cols = s.column_names();
        let rows = match s.collect_rows() {
            Ok(r) => r,
            Err(e) => {
                out.push_str(&format!("Error scanning {tbl}: {}\n", e.message));
                continue;
            }
        };
        drop(s);
        // Hash a canonical encoding: column name, NUL, value
        // (typed prefix + bytes), NUL, repeat. Not bit-identical
        // to sqlite3's sha3sum (which uses its own encoding), but
        // stable for our build's use.
        for row in &rows {
            for (i, v) in row.iter().enumerate() {
                hasher.update(cols.get(i).map(|s| s.as_bytes()).unwrap_or(b""));
                hasher.update(b"\0");
                match v {
                    Value::Null => hasher.update(b"N"),
                    Value::Integer(n) => {
                        hasher.update(b"I");
                        hasher.update(&n.to_le_bytes());
                    }
                    Value::Real(r) => {
                        hasher.update(b"R");
                        hasher.update(&r.to_le_bytes());
                    }
                    Value::Text(t) => {
                        hasher.update(b"T");
                        hasher.update(t.as_bytes());
                    }
                    Value::Blob(b) => {
                        hasher.update(b"B");
                        hasher.update(b);
                    }
                }
                hasher.update(b"\0");
            }
            hasher.update(b"\x01");
        }
        let digest = hasher.finalize();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        out.push_str(&format!("{hex}  {tbl}\n"));
    }
    out
}

fn cmd_vfslist() -> String {
    let names = Connection::list_vfses();
    if names.is_empty() {
        return "(no VFSes registered)\n".to_string();
    }
    let mut out = String::new();
    for (i, name) in names.iter().enumerate() {
        let marker = if i == 0 { " (default)" } else { "" };
        out.push_str(&format!("{name}{marker}\n"));
    }
    out
}

/// `.archive` — SQLAR (SQLite Archive Format) operations. Storage
/// is a `sqlar(name, mode, mtime, sz, data)` table inside any
/// SQLite database; we use the current connection by default or
/// open `--file FILE` instead. Compression: zlib deflate when it
/// shrinks the file (sz != length(data) → compressed and sz is
/// the original size; sz == length(data) → stored uncompressed).
fn cmd_archive(arg: &str, conn: &Connection) -> String {
    use crate::db;
    use miniz_oxide::deflate::compress_to_vec_zlib;
    use miniz_oxide::inflate::decompress_to_vec_zlib;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Op { List, Extract, Create, Update }
    let mut op = Op::List;
    let mut file: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut toks = arg.split_whitespace().peekable();
    while let Some(t) = toks.next() {
        match t {
            "--list" | "-t" => op = Op::List,
            "--extract" | "-x" => op = Op::Extract,
            "--create" | "-c" => op = Op::Create,
            "--update" | "-u" => op = Op::Update,
            "--file" | "-f" => { file = toks.next().map(|s| s.to_string()); }
            "--directory" | "-C" => { dir = toks.next().map(|s| s.to_string()); }
            other if other.starts_with("--file=") => file = Some(other[7..].to_string()),
            other if other.starts_with("--directory=") => dir = Some(other[12..].to_string()),
            other => positional.push(other.to_string()),
        }
    }

    // Where the sqlar table lives. With --file, open a fresh
    // connection to that path. Without, use the cli's main db
    // (the `conn` arg).
    let owned_conn: Option<db::Connection> = match &file {
        Some(p) => match db::Connection::open(p, db::OpenFlags::DEFAULT) {
            Ok(c) => Some(c),
            Err(e) => return format!("Error opening {p}: {}\n", e.message),
        },
        None => None,
    };
    let target = owned_conn.as_ref().unwrap_or(conn);

    // Ensure the sqlar table exists for ops that need it.
    let needs_table = matches!(op, Op::Create | Op::Update);
    if needs_table {
        if let Err(e) = target.execute_batch(
            "CREATE TABLE IF NOT EXISTS sqlar(\
               name TEXT PRIMARY KEY, mode INT, mtime INT, sz INT, data BLOB\
             ) WITHOUT ROWID"
        ) {
            return format!("Error: {}\n", e.message);
        }
    }

    match op {
        Op::List => {
            // Pattern: SELECT names matching any positional glob;
            // empty positional means everything.
            let mut out = String::new();
            let sql = if positional.is_empty() {
                "SELECT name, sz FROM sqlar ORDER BY name".to_string()
            } else {
                // glob OR chain
                let preds = positional.iter()
                    .map(|_| "name GLOB ?")
                    .collect::<Vec<_>>().join(" OR ");
                format!("SELECT name, sz FROM sqlar WHERE {preds} ORDER BY name")
            };
            let mut stmt = match target.prepare(&sql) {
                Ok(s) => s,
                Err(e) => return format!("Error: {}\n", e.message),
            };
            let params: Vec<db::Value> = positional.iter()
                .map(|p| db::Value::Text(p.clone()))
                .collect();
            if let Err(e) = stmt.bind_all(&params) {
                return format!("Error: {}\n", e.message);
            }
            let rows = match stmt.collect_rows() {
                Ok(r) => r,
                Err(e) => return format!("Error: {}\n", e.message),
            };
            for row in rows {
                if let (Some(db::Value::Text(name)), Some(db::Value::Integer(sz))) =
                    (row.get(0), row.get(1))
                {
                    out.push_str(&format!("{sz:>10}  {name}\n"));
                }
            }
            if out.is_empty() { "(archive empty)\n".to_string() } else { out }
        }
        Op::Extract => {
            // For each row matching positional globs (or all), write
            // (decompressing first if sz != length(data)) to
            // `dir/name`. Directory parents are created.
            let dir = dir.as_deref().unwrap_or(".");
            let sql = if positional.is_empty() {
                "SELECT name, sz, data FROM sqlar".to_string()
            } else {
                let preds = positional.iter()
                    .map(|_| "name GLOB ?")
                    .collect::<Vec<_>>().join(" OR ");
                format!("SELECT name, sz, data FROM sqlar WHERE {preds}")
            };
            let mut stmt = match target.prepare(&sql) {
                Ok(s) => s,
                Err(e) => return format!("Error: {}\n", e.message),
            };
            let params: Vec<db::Value> = positional.iter()
                .map(|p| db::Value::Text(p.clone()))
                .collect();
            if let Err(e) = stmt.bind_all(&params) {
                return format!("Error: {}\n", e.message);
            }
            let rows = match stmt.collect_rows() {
                Ok(r) => r,
                Err(e) => return format!("Error: {}\n", e.message),
            };
            let mut out = String::new();
            let mut count = 0;
            for row in rows {
                let name = match row.get(0) {
                    Some(db::Value::Text(s)) => s.clone(),
                    _ => continue,
                };
                let sz = match row.get(1) {
                    Some(db::Value::Integer(n)) => *n as usize,
                    _ => continue,
                };
                let data: Vec<u8> = match row.get(2) {
                    Some(db::Value::Blob(b)) => b.clone(),
                    _ => continue,
                };
                let payload: Vec<u8> = if sz == data.len() {
                    data
                } else {
                    match decompress_to_vec_zlib(&data) {
                        Ok(d) => d,
                        Err(e) => {
                            out.push_str(&format!("Error decompressing {name}: {e:?}\n"));
                            continue;
                        }
                    }
                };
                // Names in the archive can be absolute paths the
                // user typed; Path::join with an absolute right side
                // REPLACES the left, so /tmp/extract joined with
                // /tmp/files/foo.txt becomes /tmp/files/foo.txt and
                // overwrites the original. Strip leading `/` so the
                // name is always treated as relative to --directory.
                let rel_name = name.trim_start_matches('/');
                let target_path = std::path::Path::new(dir).join(rel_name);
                if let Some(parent) = target_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&target_path, &payload) {
                    out.push_str(&format!("Error writing {}: {e}\n", target_path.display()));
                    continue;
                }
                count += 1;
            }
            out.push_str(&format!("Extracted {count} file(s) to {dir}\n"));
            out
        }
        Op::Create | Op::Update => {
            if positional.is_empty() {
                return "Usage: .archive --create FILE [FILES...]\n".to_string();
            }
            if op == Op::Create {
                // wipe the sqlar table — sqlite3's --create rebuilds.
                if let Err(e) = target.execute_batch("DELETE FROM sqlar") {
                    return format!("Error: {}\n", e.message);
                }
            }
            let mut stmt = match target.prepare(
                "INSERT OR REPLACE INTO sqlar(name, mode, mtime, sz, data) \
                 VALUES (?, ?, ?, ?, ?)"
            ) {
                Ok(s) => s,
                Err(e) => return format!("Error: {}\n", e.message),
            };
            let mut count = 0u64;
            let mut errors = String::new();
            for fname in &positional {
                let raw = match std::fs::read(fname) {
                    Ok(b) => b,
                    Err(e) => {
                        errors.push_str(&format!("Error reading {fname}: {e}\n"));
                        continue;
                    }
                };
                let raw_len = raw.len() as i64;
                // miniz_oxide level 6 ≈ zlib default. Faster levels
                // available; this trades a little speed for size.
                let compressed = compress_to_vec_zlib(&raw, 6);
                let (data, sz) = if compressed.len() < raw.len() {
                    (compressed, raw_len)
                } else {
                    (raw, raw_len) // when sz == length(data), uncompressed by sqlar convention
                };
                let bindings = [
                    db::Value::Text(fname.clone()),
                    db::Value::Integer(0o100644), // generic regular-file mode
                    db::Value::Integer(0),        // mtime — we don't have wall clock yet
                    db::Value::Integer(sz),
                    db::Value::Blob(data),
                ];
                if let Err(e) = stmt.reset() {
                    errors.push_str(&format!("Error: {}\n", e.message));
                    continue;
                }
                if let Err(e) = stmt.bind_all(&bindings) {
                    errors.push_str(&format!("Error: {}\n", e.message));
                    continue;
                }
                loop {
                    match stmt.step() {
                        Ok(db::StepResult::Row) => continue,
                        Ok(db::StepResult::Done) => break,
                        Err(e) => {
                            errors.push_str(&format!("Error inserting {fname}: {}\n", e.message));
                            break;
                        }
                    }
                }
                count += 1;
            }
            let mut out = String::new();
            if !errors.is_empty() {
                out.push_str(&errors);
            }
            out.push_str(&format!(
                "{} {count} file(s) into sqlar\n",
                if op == Op::Create { "Archived" } else { "Updated" }
            ));
            out
        }
    }
}

fn cmd_vfsname(arg: &str, conn: &Connection) -> String {
    let db_name = if arg.is_empty() { "main" } else { arg.trim() };
    match conn.vfs_name(db_name) {
        Ok(name) => {
            if name.is_empty() {
                format!("(no vfs name for {db_name})\n")
            } else {
                format!("{name}\n")
            }
        }
        Err(e) => format!("Error: {}\n", e.message),
    }
}

fn cmd_fullschema(conn: &Connection) -> String {
    let mut out = String::new();
    // 1) Schema: every CREATE that the user wrote.
    match query_text_col(
        conn,
        "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid",
        &[],
    ) {
        Ok(rows) => {
            for sql in rows {
                out.push_str(&sql);
                out.push_str(";\n");
            }
        }
        Err(e) => return e,
    }
    // 2) sqlite_stat1 if it exists (ANALYZE has run).
    if let Ok(rows) = query_text_col(
        conn,
        "SELECT name FROM sqlite_master WHERE name='sqlite_stat1'",
        &[],
    ) {
        if !rows.is_empty() {
            out.push_str("ANALYZE sqlite_master;\n");
            match query_text_col(
                conn,
                "SELECT 'INSERT INTO sqlite_stat1 VALUES(' || quote(tbl) || ',' || \
                     quote(idx) || ',' || quote(stat) || ')' FROM sqlite_stat1",
                &[],
            ) {
                Ok(inserts) => {
                    for ins in inserts {
                        out.push_str(&ins);
                        out.push_str(";\n");
                    }
                }
                Err(_) => {}
            }
        }
    }
    out
}

fn cmd_dbinfo(_arg: &str, conn: &Connection) -> String {
    // sqlite3's .dbinfo is rich (parses the db header directly).
    // v1 captures the user-relevant subset via PRAGMAs.
    let probes: &[(&str, &str)] = &[
        ("page size", "PRAGMA page_size"),
        ("page count", "PRAGMA page_count"),
        ("freelist count", "PRAGMA freelist_count"),
        ("encoding", "PRAGMA encoding"),
        ("user version", "PRAGMA user_version"),
        ("application id", "PRAGMA application_id"),
        ("journal mode", "PRAGMA journal_mode"),
        ("synchronous", "PRAGMA synchronous"),
        ("auto vacuum", "PRAGMA auto_vacuum"),
    ];
    let mut out = String::new();
    for (label, sql) in probes {
        match query_text_col(conn, sql, &[]) {
            Ok(rows) => {
                if let Some(v) = rows.into_iter().next() {
                    out.push_str(&format!("{label:<18}{v}\n"));
                }
            }
            Err(_) => {}
        }
    }
    out
}

/// Map of recognized SQLITE_DBCONFIG_* boolean options. Each entry
/// is (cli-facing name, ffi constant). Names match sqlite3's
/// `.dbconfig` exactly so scripts port across.
const DBCONFIG_BOOLEANS: &[(&str, c_int)] = &[
    ("defensive", ffi::SQLITE_DBCONFIG_DEFENSIVE as c_int),
    ("dqs_dml", ffi::SQLITE_DBCONFIG_DQS_DML as c_int),
    ("dqs_ddl", ffi::SQLITE_DBCONFIG_DQS_DDL as c_int),
    ("enable_fkey", ffi::SQLITE_DBCONFIG_ENABLE_FKEY as c_int),
    ("enable_trigger", ffi::SQLITE_DBCONFIG_ENABLE_TRIGGER as c_int),
    ("enable_view", ffi::SQLITE_DBCONFIG_ENABLE_VIEW as c_int),
    ("enable_load_extension", ffi::SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION as c_int),
    ("enable_qpsg", ffi::SQLITE_DBCONFIG_ENABLE_QPSG as c_int),
    ("legacy_alter_table", ffi::SQLITE_DBCONFIG_LEGACY_ALTER_TABLE as c_int),
    ("legacy_file_format", ffi::SQLITE_DBCONFIG_LEGACY_FILE_FORMAT as c_int),
    ("trigger_eqp", ffi::SQLITE_DBCONFIG_TRIGGER_EQP as c_int),
    ("trusted_schema", ffi::SQLITE_DBCONFIG_TRUSTED_SCHEMA as c_int),
    ("writable_schema", ffi::SQLITE_DBCONFIG_WRITABLE_SCHEMA as c_int),
];

fn cmd_dbconfig(arg: &str, conn: &Connection) -> String {
    let mut parts = arg.split_whitespace();
    let op = parts.next().unwrap_or("");
    let val = parts.next().unwrap_or("");
    if op.is_empty() {
        // List every known boolean and its current value.
        let mut out = String::new();
        for (name, code) in DBCONFIG_BOOLEANS {
            match conn.db_config_get_bool(*code) {
                Ok(b) => out.push_str(&format!("{:>22} {}\n", name, b as i32)),
                Err(_) => {}
            }
        }
        return out;
    }
    let entry = DBCONFIG_BOOLEANS.iter().find(|(n, _)| *n == op);
    let (_, code) = match entry {
        Some(e) => e,
        None => return format!("Error: unknown dbconfig op: {op}\n"),
    };
    if val.is_empty() {
        match conn.db_config_get_bool(*code) {
            Ok(b) => format!("{op} {}\n", b as i32),
            Err(e) => format!("Error: {}\n", e.message),
        }
    } else {
        let on = parse_on_off(val);
        match conn.db_config_set_bool(*code, on) {
            Ok(b) => format!("{op} {}\n", b as i32),
            Err(e) => format!("Error: {}\n", e.message),
        }
    }
}

/// Map of recognized SQLITE_LIMIT_* categories.
const LIMIT_NAMES: &[(&str, c_int)] = &[
    ("length", ffi::SQLITE_LIMIT_LENGTH),
    ("sql_length", ffi::SQLITE_LIMIT_SQL_LENGTH),
    ("column", ffi::SQLITE_LIMIT_COLUMN),
    ("expr_depth", ffi::SQLITE_LIMIT_EXPR_DEPTH),
    ("compound_select", ffi::SQLITE_LIMIT_COMPOUND_SELECT),
    ("vdbe_op", ffi::SQLITE_LIMIT_VDBE_OP),
    ("function_arg", ffi::SQLITE_LIMIT_FUNCTION_ARG),
    ("attached", ffi::SQLITE_LIMIT_ATTACHED),
    ("like_pattern_length", ffi::SQLITE_LIMIT_LIKE_PATTERN_LENGTH),
    ("variable_number", ffi::SQLITE_LIMIT_VARIABLE_NUMBER),
    ("trigger_depth", ffi::SQLITE_LIMIT_TRIGGER_DEPTH),
    ("worker_threads", ffi::SQLITE_LIMIT_WORKER_THREADS),
];

fn cmd_limit(arg: &str, conn: &Connection) -> String {
    let mut parts = arg.split_whitespace();
    let name = parts.next().unwrap_or("");
    let val = parts.next().unwrap_or("");
    if name.is_empty() {
        let mut out = String::new();
        for (n, code) in LIMIT_NAMES {
            let v = conn.limit(*code, -1);
            out.push_str(&format!("{:>22} {v}\n", n));
        }
        return out;
    }
    let entry = LIMIT_NAMES.iter().find(|(n, _)| *n == name);
    let (_, code) = match entry {
        Some(e) => e,
        None => return format!("Error: unknown limit: {name}\n"),
    };
    if val.is_empty() {
        let v = conn.limit(*code, -1);
        format!("{name} {v}\n")
    } else {
        match val.parse::<i32>() {
            Ok(n) => {
                let prev = conn.limit(*code, n);
                format!("{name} {prev} -> {}\n", conn.limit(*code, -1))
            }
            Err(_) => format!("Usage: .limit {name} N\n"),
        }
    }
}

// .binary  moved to extensions/core-dotcmd (Phase 2.2).

fn cmd_timeout(arg: &str, conn: &Connection) -> String {
    if arg.is_empty() {
        return "Usage: .timeout MS\n".to_string();
    }
    let ms: i32 = match arg.parse() {
        Ok(n) => n,
        Err(_) => return format!("Usage: .timeout MS (got {arg:?})\n"),
    };
    match conn.busy_timeout(ms) {
        Ok(()) => String::new(),
        Err(e) => format!("Error: {}\n", e.message),
    }
}

// .explain / .eqp / .stats  moved to extensions/core-dotcmd (Phase 2.2).

fn cmd_parameter(arg: &str) -> String {
    let mut parts = arg.splitn(3, char::is_whitespace);
    let sub = parts.next().unwrap_or("").trim();
    match sub {
        "" => "Usage: .parameter init|list|set NAME VALUE|clear|unset NAME\n".to_string(),
        "init" | "clear" => {
            settings::SETTINGS.with(|s| s.borrow_mut().parameters.clear());
            String::new()
        }
        "list" => {
            settings::SETTINGS.with(|s| {
                let g = s.borrow();
                if g.parameters.is_empty() {
                    return "(no parameters)\n".to_string();
                }
                let mut names: Vec<&String> = g.parameters.keys().collect();
                names.sort();
                let mut out = String::new();
                for n in names {
                    let v = g.parameters.get(n).unwrap();
                    out.push_str(&format!("{n} = {}\n", crate::db_value_display(v)));
                }
                out
            })
        }
        "set" => {
            let name = parts.next().unwrap_or("").trim();
            let value = parts.next().unwrap_or("").trim();
            if name.is_empty() || value.is_empty() {
                return "Usage: .parameter set NAME VALUE\n".to_string();
            }
            let bare = strip_param_sigil(name).to_string();
            let v = parse_parameter_value(value);
            settings::SETTINGS.with(|s| {
                s.borrow_mut().parameters.insert(bare, v);
            });
            String::new()
        }
        "unset" => {
            let name = parts.next().unwrap_or("").trim();
            if name.is_empty() {
                return "Usage: .parameter unset NAME\n".to_string();
            }
            let bare = strip_param_sigil(name).to_string();
            settings::SETTINGS.with(|s| {
                s.borrow_mut().parameters.remove(&bare);
            });
            String::new()
        }
        _ => "Usage: .parameter init|list|set NAME VALUE|clear|unset NAME\n".to_string(),
    }
}

/// Accept names with or without a leading `:` / `$` / `@`; store
/// the bare name in Settings.parameters so lookup against
/// sqlite3_bind_parameter_name's sigil-prefixed form works
/// regardless of which form the user typed.
fn strip_param_sigil(name: &str) -> &str {
    match name.as_bytes().first() {
        Some(b':') | Some(b'$') | Some(b'@') => &name[1..],
        _ => name,
    }
}

/// Crude scalar parse for `.parameter set`: integer first, then
/// real, then text (everything else). Numbers in quotes are treated
/// as text. NULL keyword maps to Value::Null.
fn parse_parameter_value(raw: &str) -> Value {
    if raw.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    // Quoted text — strip outer quotes (single or double); unescape
    // doubled quotes inside.
    if raw.len() >= 2 {
        let bytes = raw.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'\'' || first == b'"') && first == last {
            let inner = &raw[1..raw.len() - 1];
            let unesc = if first == b'\'' {
                inner.replace("''", "'")
            } else {
                inner.replace("\"\"", "\"")
            };
            return Value::Text(unesc);
        }
    }
    if let Ok(n) = raw.parse::<i64>() {
        return Value::Integer(n);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return Value::Real(f);
    }
    Value::Text(raw.to_string())
}

fn cmd_version() -> String {
    let lib = crate::db::version();
    let pkg = env!("CARGO_PKG_VERSION");
    format!(
        "SQLite {lib}\nsqlite-cli (Rust, wasm32-wasip2) {pkg}\n"
    )
}

fn cmd_width(arg: &str) -> String {
    if arg.is_empty() {
        settings::SETTINGS.with(|s| s.borrow_mut().column_widths.clear());
        return String::new();
    }
    let mut widths = Vec::new();
    for tok in arg.split_whitespace() {
        match tok.parse::<isize>() {
            Ok(n) => widths.push(n.max(0) as usize),
            Err(_) => return format!("Usage: .width N N ...\n"),
        }
    }
    settings::SETTINGS.with(|s| s.borrow_mut().column_widths = widths);
    String::new()
}

// .changes / .timer  moved to extensions/core-dotcmd (Phase 2.2).

fn cmd_help() -> String {
    let mut o = String::new();
    o.push_str(".bail on|off            Stop on first error\n");
    o.push_str(".databases              List attached databases\n");
    o.push_str(".echo on|off            Echo SQL before executing\n");
    o.push_str(".exit | .quit           Exit the CLI\n");
    o.push_str(".headers on|off         Show column headers\n");
    o.push_str(".help                   This message\n");
    o.push_str(".indexes ?TABLE?        List indexes\n");
    o.push_str(".load FILE [GRANTS]     Load a WASM extension\n");
    o.push_str(".mode MODE              list|csv|line|column|table|markdown|tabs|json\n");
    o.push_str(".nullvalue STR          What to print for NULL (default: empty)\n");
    o.push_str(".print STR...           Print arg verbatim\n");
    o.push_str(".prompt MAIN CONT       Set prompts\n");
    o.push_str(".schema ?TABLE?         Show CREATE statements\n");
    o.push_str(".separator STR          Column separator (list/csv modes)\n");
    o.push_str(".show                   Show current settings\n");
    o.push_str(".tables ?PATTERN?       List tables matching pattern\n");
    o.push_str(".fiji FILE              Run a Fiji function (compose-shaped wasm)\n");
    o.push_str(".register-provider ID FILE  Register a wasm-component compose provider\n");
    o.push_str(".register-resolver SCHEME FILE  Register a URI resolver\n");
    o.push_str(".unregister-resolver SCHEME  Drop a registered resolver\n");
    o.push_str(".resolvers              List registered resolvers\n");
    o.push_str(".cache [purge|list]     CAS cache control\n");
    o
}

fn cmd_show() -> String {
    let s = settings::SETTINGS.with(|s| s.borrow().clone());
    let mut o = String::new();
    o.push_str(&format!("        echo: {}\n", on_off(s.echo)));
    o.push_str(&format!("        bail: {}\n", on_off(s.bail)));
    o.push_str(&format!("     headers: {}\n", on_off(s.headers)));
    o.push_str(&format!("        mode: {}\n", s.mode.name()));
    o.push_str(&format!("   nullvalue: {:?}\n", s.null_value));
    o.push_str(&format!("   separator: {:?}\n", s.separator));
    o.push_str(&format!("      prompt: {:?}\n", s.prompt_main));
    o.push_str(&format!("contprompt: {:?}\n", s.prompt_cont));
    o
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn cmd_tables(arg: &str, conn: &Connection) -> String {
    let pattern = if arg.is_empty() { "%" } else { arg };
    let sql = "SELECT name FROM sqlite_master \
               WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' AND name LIKE ?1 \
               ORDER BY name";
    match query_text_col(conn, sql, &[Value::Text(pattern.to_string())]) {
        Ok(names) => {
            if names.is_empty() {
                String::new()
            } else {
                names.join("\n") + "\n"
            }
        }
        Err(e) => e,
    }
}

fn cmd_schema(arg: &str, conn: &Connection) -> String {
    let (sql, params): (&str, Vec<Value>) = if arg.is_empty() {
        (
            "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type, name",
            vec![],
        )
    } else {
        (
            "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL AND name = ?1",
            vec![Value::Text(arg.to_string())],
        )
    };
    match query_text_col(conn, sql, &params) {
        Ok(sqls) => {
            let mut out = String::new();
            for sql in sqls {
                out.push_str(&sql);
                if !sql.ends_with(';') {
                    out.push(';');
                }
                out.push('\n');
            }
            out
        }
        Err(e) => e,
    }
}

fn cmd_indexes(arg: &str, conn: &Connection) -> String {
    let (sql, params): (&str, Vec<Value>) = if arg.is_empty() {
        (
            "SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name",
            vec![],
        )
    } else {
        (
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 ORDER BY name",
            vec![Value::Text(arg.to_string())],
        )
    };
    match query_text_col(conn, sql, &params) {
        Ok(names) => {
            if names.is_empty() {
                String::new()
            } else {
                names.join("\n") + "\n"
            }
        }
        Err(e) => e,
    }
}

fn cmd_databases(conn: &Connection) -> String {
    let mut stmt = match conn.prepare("PRAGMA database_list") {
        Ok(s) => s,
        Err(e) => return format!("Error: {}\n", e.message),
    };
    let rows = match stmt.collect_rows() {
        Ok(r) => r,
        Err(e) => return format!("Error: {}\n", e.message),
    };
    let mut out = String::new();
    for r in rows {
        let seq = match r.first() {
            Some(Value::Integer(i)) => *i,
            _ => 0,
        };
        let name = match r.get(1) {
            Some(Value::Text(s)) => s.clone(),
            _ => String::new(),
        };
        let file = match r.get(2) {
            Some(Value::Text(s)) => s.clone(),
            _ => String::new(),
        };
        out.push_str(&format!("{seq}: {name} -> {file}\n"));
    }
    out
}

// .headers / .mode / .nullvalue / .separator / .echo / .prompt
// / .bail  moved to extensions/core-dotcmd (Phase 2.2). The cli
// applies the returned state-deltas via settings::apply_dotcmd_delta.

fn parse_on_off(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "on" | "true" | "1" | "yes")
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2) {
        s[1..s.len()-1].to_string()
    } else {
        s.to_string()
    }
}

#[allow(dead_code)]
fn _unused() { let _: RefCell<()> = RefCell::new(()); }

// ---------------------------------------------------------------
// .sqlink  Phase 3 meta-cli (PLAN-dotcmd-plugins.md Layer 2).
//
// Subcommands:
//
//   .sqlink list                         every registered command
//   .sqlink show NAME                    full row + manifest snippet
//   .sqlink install URI [--bundle]       file://PATH today (Phase 4: http/cas)
//   .sqlink uninstall NAME               delete the row (artifact stays)
//
// `install` reads bytes off the local fs, hashes them with blake3
// for the artifact_digest, calls
// `extension-loader.load-extension-from-bytes` so the manifest comes
// back AND the session registry sees the extension immediately, then
// INSERTs one row per `manifest.dot_commands` entry. `--bundle`
// (default true for file://) also stores the bytes in
// `sqlink_artifact` so future runs can resolve without the file.
//
// Lives in dot.rs because it needs `extension-loader` host access
// (which the dotcmd-aware world doesn't expose). Plan calls for
// a separate `extensions/sqlink-meta-cli/` extension once the world
// is widened  see open question in PLAN-dotcmd-plugins.md.
fn cmd_sqlink(arg: &str, conn: &crate::db::Connection) -> String {
    let mut parts = arg.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();
    match sub {
        "" => sqlink_usage(),
        "list" => sqlink_list(conn),
        "show" => sqlink_show(conn, rest),
        "install" => sqlink_install(conn, rest),
        "uninstall" => sqlink_uninstall(conn, rest),
        "bundle" => sqlink_bundle(conn, rest),
        "unbundle" => sqlink_unbundle(conn, rest),
        "bundle-all" => sqlink_bundle_all(conn),
        "unbundle-all" => sqlink_unbundle_all(conn),
        "verify" => sqlink_verify(conn),
        "gc" => sqlink_gc(conn),
        "export" => sqlink_export(conn, rest),
        "resolver" => sqlink_resolver(conn, rest),
        other => format!(".sqlink: unknown subcommand {other:?}\n{}", sqlink_usage()),
    }
}

fn sqlink_usage() -> String {
    "Usage:\n  \
        .sqlink list\n  \
        .sqlink show NAME\n  \
        .sqlink install URI [--no-bundle]\n  \
        .sqlink uninstall NAME\n  \
        .sqlink bundle NAME\n  \
        .sqlink unbundle NAME\n  \
        .sqlink bundle-all\n  \
        .sqlink unbundle-all\n  \
        .sqlink verify\n  \
        .sqlink gc\n  \
        .sqlink export NAME PATH\n  \
        .sqlink resolver list\n  \
        .sqlink resolver add PRIORITY URI\n  \
        .sqlink resolver remove URI\n  \
        .sqlink resolver set-priority URI N\n"
        .to_string()
}

fn sqlink_list(conn: &crate::db::Connection) -> String {
    let rows = crate::sqlink_registry::list_rows(conn);
    if rows.is_empty() {
        return "(no commands installed)\n".to_string();
    }
    let mut out = String::new();
    out.push_str("NAME              BUNDLED   SIZE    SUMMARY\n");
    for (name, summary, size, _digest, bundled) in rows {
        out.push_str(&format!(
            "{name:<17} {b:<8} {size:<7} {summary}\n",
            b = if bundled { "yes" } else { "no" },
        ));
    }
    out
}

fn sqlink_show(conn: &crate::db::Connection, name: &str) -> String {
    if name.is_empty() {
        return "Usage: .sqlink show NAME\n".to_string();
    }
    let Some(row) = crate::sqlink_registry::show_row(conn, name) else {
        return format!("(no row for {name:?})\n");
    };
    let mut o = String::new();
    o.push_str(&format!("name:         {}\n", row.name));
    o.push_str(&format!("summary:      {}\n", row.summary));
    if !row.help.is_empty() {
        o.push_str(&format!("help:         {}\n", row.help));
    }
    o.push_str(&format!("digest:       {}\n", row.digest));
    o.push_str(&format!("size:         {} bytes\n", row.size));
    o.push_str(&format!("installed_at: {}\n", row.installed_at));
    o.push_str(&format!("source_uri:   {}\n",
        if row.source_uri.is_empty() { "(none)" } else { &row.source_uri }));
    o.push_str(&format!("bundled:      {}\n", if row.bundled { "yes" } else { "no" }));
    o
}

fn sqlink_install(conn: &crate::db::Connection, arg: &str) -> String {
    let mut bundle = true;
    let mut uri: Option<&str> = None;
    for tok in arg.split_whitespace() {
        match tok {
            "--no-bundle" => bundle = false,
            "--bundle" => bundle = true,
            other if !other.starts_with("--") => uri = Some(other),
            other => return format!(".sqlink install: unknown flag {other:?}\n"),
        }
    }
    let Some(uri) = uri else {
        return "Usage: .sqlink install URI [--no-bundle]\n".to_string();
    };

    // v1: only file:// is wired host-side.
    let path: String = if let Some(p) = uri.strip_prefix("file://") {
        p.to_string()
    } else if uri.starts_with("http://") || uri.starts_with("https://") {
        return ".sqlink install: http(s) URIs deferred to Phase 4 (external CAS)\n".to_string();
    } else if uri.starts_with("cas:") {
        return ".sqlink install: cas: URIs deferred to Phase 4 (external CAS)\n".to_string();
    } else {
        // Bare path  treat as a local file.
        uri.to_string()
    };

    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return format!(".sqlink install: read {path:?}: {e}\n"),
    };
    let digest_hex = blake3::hash(&bytes).to_hex().to_string();
    let digest = format!("blake3:{digest_hex}");
    let size = bytes.len() as i64;

    // Load the bytes through the host  this gives us the manifest
    // and populates the session registry simultaneously, so the
    // newly-installed command is callable in this very session.
    use crate::bindings::sqlite::wasm::extension_loader;
    let opts = crate::bindings::sqlite::extension::policy::LoadOptions {
        grant: Vec::new(),
        http_policy: None,
        dns_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    let manifest = match extension_loader::load_extension_from_bytes("", &bytes, &opts) {
        Ok(m) => m,
        Err(e) => return format!(".sqlink install: load failed: {} ({})\n", e.message, e.code),
    };

    if manifest.dot_commands.is_empty() {
        return format!(
            ".sqlink install: {} declares no dot commands (loaded but not registered in sqlink_dotcmd)\n",
            manifest.name,
        );
    }

    let mut installed: Vec<String> = Vec::new();
    let mut errs: Vec<String> = Vec::new();
    for dc in &manifest.dot_commands {
        match crate::sqlink_registry::install(
            conn,
            &dc.name,
            &dc.summary,
            &dc.help,
            dc.id,
            dc.requires_write,
            &digest,
            size,
            uri,
            bundle,
            &bytes,
        ) {
            Ok(()) => installed.push(dc.name.clone()),
            Err(e) => errs.push(format!("{}: {}", dc.name, e.message)),
        }
    }
    let mut out = format!(
        "Installed {} from {} ({} bytes, digest {}):\n",
        manifest.name, uri, size, digest,
    );
    for n in &installed {
        out.push_str(&format!("  .{n}\n"));
    }
    if !errs.is_empty() {
        out.push_str("Errors:\n");
        for e in &errs {
            out.push_str(&format!("  {e}\n"));
        }
    }
    out
}

fn sqlink_uninstall(conn: &crate::db::Connection, name: &str) -> String {
    if name.is_empty() {
        return "Usage: .sqlink uninstall NAME\n".to_string();
    }
    match crate::sqlink_registry::uninstall(conn, name) {
        Ok(0) => format!("(no row for {name:?})\n"),
        Ok(n) => format!("Uninstalled {} ({n} row{})\n", name, if n == 1 { "" } else { "s" }),
        Err(e) => format!(".sqlink uninstall {name}: {}\n", e.message),
    }
}

/// `.sqlink bundle NAME`  ensure the row's artifact_digest has
/// matching bytes in sqlink_artifact. v1: re-reads from
/// `source_uri` when it's a `file://` URI. Phase 4 will fall back
/// to walking sqlink_cas_resolver for non-file sources.
fn sqlink_bundle(conn: &crate::db::Connection, name: &str) -> String {
    if name.is_empty() {
        return "Usage: .sqlink bundle NAME\n".to_string();
    }
    let Some(meta) = crate::sqlink_registry::dotcmd_meta(conn, name) else {
        return format!("(no row for {name:?})\n");
    };
    if crate::sqlink_registry::fetch_artifact(conn, &meta.digest).is_some() {
        return format!("{name}: already bundled\n");
    }
    let bytes = match try_fetch_bytes(conn, &meta.source_uri, &meta.digest) {
        FetchResult::Bytes(b) => b,
        FetchResult::NoSource => return format!(
            "{name}: source_uri empty and no CAS resolver hit  cannot bundle\n"
        ),
        FetchResult::Err(e) => return format!("{name}: {e}\n"),
    };
    let got_digest = format!("blake3:{}", blake3::hash(&bytes).to_hex());
    if got_digest != meta.digest {
        return format!(
            "{name}: digest mismatch from {} ({} != {})\n",
            meta.source_uri, got_digest, meta.digest,
        );
    }
    match crate::sqlink_registry::store_artifact(
        conn, &meta.digest, bytes.len() as i64, &bytes, &meta.source_uri,
    ) {
        Ok(()) => format!("{name}: bundled {} bytes ({})\n", bytes.len(), meta.digest),
        Err(e) => format!("{name}: store_artifact: {}\n", e.message),
    }
}

/// `.sqlink unbundle NAME`  drop the artifact iff this is the
/// last sqlink_dotcmd row pointing at it. Otherwise refuse with a
/// list of shared names (a multi-command extension stays bundled
/// until every command using it is unbundled or uninstalled).
fn sqlink_unbundle(conn: &crate::db::Connection, name: &str) -> String {
    if name.is_empty() {
        return "Usage: .sqlink unbundle NAME\n".to_string();
    }
    let Some(meta) = crate::sqlink_registry::dotcmd_meta(conn, name) else {
        return format!("(no row for {name:?})\n");
    };
    let refs = crate::sqlink_registry::digest_refcount(conn, &meta.digest);
    if refs > 1 {
        return format!(
            "{name}: artifact shared by {refs} commands  refusing to unbundle. \
             Run `.sqlink uninstall` on the others first or use `.sqlink unbundle-all`.\n"
        );
    }
    match crate::sqlink_registry::drop_artifact(conn, &meta.digest) {
        Ok(0) => format!("{name}: artifact already gone\n"),
        Ok(_) => format!("{name}: unbundled ({})\n", meta.digest),
        Err(e) => format!("{name}: drop_artifact: {}\n", e.message),
    }
}

fn sqlink_bundle_all(conn: &crate::db::Connection) -> String {
    let rows = crate::sqlink_registry::all_name_digest(conn);
    if rows.is_empty() {
        return "(no commands installed)\n".to_string();
    }
    let mut out = String::new();
    for (name, _digest) in rows {
        out.push_str(&sqlink_bundle(conn, &name));
    }
    out
}

fn sqlink_unbundle_all(conn: &crate::db::Connection) -> String {
    // unbundle-all bypasses the refcount check  the user is
    // explicitly saying "every artifact goes".
    let mut dropped = 0i64;
    let mut errs: Vec<String> = Vec::new();
    for (digest, _size) in crate::sqlink_registry::all_artifact_digests(conn) {
        match crate::sqlink_registry::drop_artifact(conn, &digest) {
            Ok(n) => dropped += n,
            Err(e) => errs.push(format!("{digest}: {}", e.message)),
        }
    }
    let mut out = format!("Dropped {dropped} artifact row{}\n",
        if dropped == 1 { "" } else { "s" });
    for e in errs { out.push_str(&format!("  {e}\n")); }
    out
}

/// `.sqlink verify`  re-hash every sqlink_artifact row's bytes
/// column and flag any digest mismatches. Doesn't touch the
/// database  it's a read-only audit.
fn sqlink_verify(conn: &crate::db::Connection) -> String {
    let rows = crate::sqlink_registry::all_artifact_digests(conn);
    if rows.is_empty() {
        return "(no artifacts)\n".to_string();
    }
    let mut ok = 0u64;
    let mut bad: Vec<String> = Vec::new();
    for (digest, size) in rows {
        let Some(bytes) = crate::sqlink_registry::fetch_artifact(conn, &digest) else {
            bad.push(format!("{digest}: fetch failed"));
            continue;
        };
        if bytes.len() as i64 != size {
            bad.push(format!(
                "{digest}: size column = {size} but blob is {} bytes",
                bytes.len(),
            ));
            continue;
        }
        let got = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        if got == digest {
            ok += 1;
        } else {
            bad.push(format!("{digest}: hashes to {got}"));
        }
    }
    let mut out = format!("verify: {ok} ok, {} bad\n", bad.len());
    for b in &bad { out.push_str(&format!("  {b}\n")); }
    out
}

fn sqlink_gc(conn: &crate::db::Connection) -> String {
    match crate::sqlink_registry::gc_artifacts(conn) {
        Ok(0) => "gc: nothing to drop\n".to_string(),
        Ok(n) => format!("gc: dropped {n} unreferenced artifact row{}\n",
            if n == 1 { "" } else { "s" }),
        Err(e) => format!("gc: {}\n", e.message),
    }
}

fn sqlink_export(conn: &crate::db::Connection, arg: &str) -> String {
    let mut parts = arg.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let path = parts.next().unwrap_or("").trim();
    if name.is_empty() || path.is_empty() {
        return "Usage: .sqlink export NAME PATH\n".to_string();
    }
    let Some(meta) = crate::sqlink_registry::dotcmd_meta(conn, name) else {
        return format!("(no row for {name:?})\n");
    };
    let Some(bytes) = crate::sqlink_registry::fetch_artifact_bytes(conn, &meta.digest) else {
        return format!("{name}: not bundled (digest {})\n", meta.digest);
    };
    match std::fs::write(path, &bytes) {
        Ok(()) => format!("Wrote {} bytes to {path}\n", bytes.len()),
        Err(e) => format!(".sqlink export {name}: {e}\n"),
    }
}

fn sqlink_resolver(conn: &crate::db::Connection, arg: &str) -> String {
    let mut parts = arg.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();
    match sub {
        "" | "list" => sqlink_resolver_list(conn),
        "add" => sqlink_resolver_add(conn, rest),
        "remove" | "rm" => sqlink_resolver_remove(conn, rest),
        "set-priority" => sqlink_resolver_set_priority(conn, rest),
        other => format!(".sqlink resolver: unknown {other:?}\n"),
    }
}

fn sqlink_resolver_list(conn: &crate::db::Connection) -> String {
    let rows = crate::sqlink_registry::resolver_list(conn);
    if rows.is_empty() {
        return "(no resolvers configured)\n".to_string();
    }
    let mut o = String::new();
    o.push_str("PRIORITY  KIND    URI\n");
    for r in rows {
        o.push_str(&format!("{:<9} {:<7} {}\n", r.priority, r.kind, r.uri));
    }
    o
}

fn sqlink_resolver_add(conn: &crate::db::Connection, arg: &str) -> String {
    let mut parts = arg.splitn(2, char::is_whitespace);
    let priority_s = parts.next().unwrap_or("").trim();
    let uri = parts.next().unwrap_or("").trim();
    if priority_s.is_empty() || uri.is_empty() {
        return "Usage: .sqlink resolver add PRIORITY URI\n".to_string();
    }
    let Ok(priority) = priority_s.parse::<i64>() else {
        return format!(".sqlink resolver add: bad priority {priority_s:?}\n");
    };
    let kind = if uri.starts_with("file://") || uri.starts_with('/') {
        "file"
    } else if uri.starts_with("http://") || uri.starts_with("https://") {
        "http"
    } else {
        return format!(".sqlink resolver add: cannot infer kind from {uri:?}\n");
    };
    match crate::sqlink_registry::resolver_add(conn, priority, kind, uri) {
        Ok(()) => format!("Added resolver: priority={priority} kind={kind} uri={uri}\n"),
        Err(e) => format!(".sqlink resolver add: {}\n", e.message),
    }
}

fn sqlink_resolver_remove(conn: &crate::db::Connection, uri: &str) -> String {
    if uri.is_empty() {
        return "Usage: .sqlink resolver remove URI\n".to_string();
    }
    match crate::sqlink_registry::resolver_remove(conn, uri) {
        Ok(0) => format!("(no resolver row for {uri:?})\n"),
        Ok(_) => format!("Removed resolver {uri}\n"),
        Err(e) => format!(".sqlink resolver remove: {}\n", e.message),
    }
}

fn sqlink_resolver_set_priority(conn: &crate::db::Connection, arg: &str) -> String {
    let mut parts = arg.splitn(2, char::is_whitespace);
    let uri = parts.next().unwrap_or("").trim();
    let n_s = parts.next().unwrap_or("").trim();
    if uri.is_empty() || n_s.is_empty() {
        return "Usage: .sqlink resolver set-priority URI N\n".to_string();
    }
    let Ok(n) = n_s.parse::<i64>() else {
        return format!(".sqlink resolver set-priority: bad N {n_s:?}\n");
    };
    match crate::sqlink_registry::resolver_set_priority(conn, uri, n) {
        Ok(0) => format!("(no resolver row for {uri:?})\n"),
        Ok(_) => format!("{uri}: priority -> {n}\n"),
        Err(e) => format!(".sqlink resolver set-priority: {}\n", e.message),
    }
}

/// Bytes-fetch helper shared by `.sqlink bundle` (re-bundle from
/// source) and the auto-resolve fallthrough (Phase 4 CAS walk).
///
/// Strategy:
///   1. If `source_uri` is `file://PATH` or a bare path, read directly.
///   2. Otherwise walk `sqlink_cas_resolver` by priority and try
///      each (`file` kind only in v1; http kind returns Err with a
///      "not yet wired" message).
///
/// `expected_digest` is provided for the CAS walk (which probes by
/// digest); the source-URI branch ignores it (caller verifies).
pub(crate) fn try_fetch_bytes(
    conn: &crate::db::Connection,
    source_uri: &str,
    expected_digest: &str,
) -> FetchResult {
    if !source_uri.is_empty() {
        let path: Option<&str> =
            if let Some(p) = source_uri.strip_prefix("file://") { Some(p) }
            else if source_uri.starts_with('/') { Some(source_uri) }
            else { None };
        if let Some(p) = path {
            return match std::fs::read(p) {
                Ok(b) => FetchResult::Bytes(b),
                Err(e) => FetchResult::Err(format!("read {p:?}: {e}")),
            };
        }
        // For non-file source_uri we still want to try CAS  fall
        // through.
    }
    walk_cas_resolvers(conn, expected_digest)
}

pub(crate) enum FetchResult {
    Bytes(Vec<u8>),
    NoSource,
    Err(String),
}

/// Phase 4 CAS walk. For each `sqlink_cas_resolver` row in
/// priority order, try to fetch the artifact by digest and return
/// the first bytes that hash to the expected digest.
///
/// v1 supports the `file` kind only  probes
/// `ROOT/blake3/AA/REST` where `digest = "blake3:AAREST..."`.
/// `http` kind logs and skips (TODO).
pub(crate) fn walk_cas_resolvers(
    conn: &crate::db::Connection,
    expected_digest: &str,
) -> FetchResult {
    let Some(hex) = expected_digest.strip_prefix("blake3:") else {
        return FetchResult::Err(format!(
            "unsupported digest scheme {expected_digest:?}  expected blake3:HEX",
        ));
    };
    if hex.len() < 3 {
        return FetchResult::Err(format!("digest too short: {expected_digest:?}"));
    }
    let resolvers = crate::sqlink_registry::resolver_list(conn);
    if resolvers.is_empty() {
        return FetchResult::NoSource;
    }
    let (aa, rest) = hex.split_at(2);
    let mut errs: Vec<String> = Vec::new();
    for r in resolvers {
        let bytes_opt: Option<Vec<u8>> = match r.kind.as_str() {
            "file" => {
                let root = r.uri.strip_prefix("file://").unwrap_or(&r.uri);
                let path = format!("{root}/blake3/{aa}/{rest}");
                match std::fs::read(&path) {
                    Ok(b) => Some(b),
                    Err(e) => { errs.push(format!("{}: {e}", path)); None }
                }
            }
            "http" => {
                // Phase 4 http-CAS: route through the host. The cli's
                // wasm sandbox doesn't speak the network itself; the
                // host has reqwest + TLS + DNS configured already.
                // The host also blake3-verifies, but we re-check here
                // (defense in depth + clearer error path).
                let trimmed = r.uri.trim_end_matches('/');
                let probe = format!("{trimmed}/blake3/{aa}/{rest}");
                use crate::bindings::sqlite::wasm::extension_loader;
                match extension_loader::fetch_cas_uri(&probe, expected_digest) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        errs.push(format!("{probe}: {} ({})", e.message, e.code));
                        None
                    }
                }
            }
            other => { errs.push(format!("{}: unknown kind {other:?}", r.uri)); None }
        };
        if let Some(bytes) = bytes_opt {
            let got = format!("blake3:{}", blake3::hash(&bytes).to_hex());
            if got == expected_digest {
                return FetchResult::Bytes(bytes);
            } else {
                errs.push(format!("{}: digest mismatch ({})", r.uri, got));
            }
        }
    }
    if errs.is_empty() { FetchResult::NoSource } else { FetchResult::Err(errs.join("; ")) }
}
