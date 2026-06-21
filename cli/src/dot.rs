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
use crate::settings::{self, Mode};

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

fn cmd_binary(arg: &str) -> String {
    if arg.is_empty() {
        let on = settings::SETTINGS.with(|s| s.borrow().binary_output);
        return format!("binary: {}\n", if on { "on" } else { "off" });
    }
    let on = parse_on_off(arg);
    settings::SETTINGS.with(|s| s.borrow_mut().binary_output = on);
    String::new()
}

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

fn cmd_explain(arg: &str) -> String {
    use crate::settings::ExplainMode;
    let mode = match arg {
        "" => {
            let m = settings::SETTINGS.with(|s| s.borrow().explain_mode);
            let name = match m {
                ExplainMode::Off => "off",
                ExplainMode::On => "on",
                ExplainMode::Auto => "auto",
            };
            return format!("explain: {name}\n");
        }
        "on" => ExplainMode::On,
        "off" => ExplainMode::Off,
        "auto" => ExplainMode::Auto,
        _ => return "Usage: .explain on|off|auto\n".to_string(),
    };
    settings::SETTINGS.with(|s| s.borrow_mut().explain_mode = mode);
    String::new()
}

fn cmd_eqp(arg: &str) -> String {
    if arg.is_empty() {
        let on = settings::SETTINGS.with(|s| s.borrow().eqp);
        return format!("eqp: {}\n", if on { "on" } else { "off" });
    }
    let on = parse_on_off(arg);
    settings::SETTINGS.with(|s| s.borrow_mut().eqp = on);
    String::new()
}

fn cmd_stats(arg: &str) -> String {
    if arg.is_empty() {
        let on = settings::SETTINGS.with(|s| s.borrow().show_stats);
        return format!("stats: {}\n", if on { "on" } else { "off" });
    }
    let on = parse_on_off(arg);
    settings::SETTINGS.with(|s| s.borrow_mut().show_stats = on);
    String::new()
}

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

fn cmd_changes(arg: &str) -> String {
    if arg.is_empty() {
        let on = settings::SETTINGS.with(|s| s.borrow().show_changes);
        return format!("changes: {}\n", if on { "on" } else { "off" });
    }
    let on = parse_on_off(arg);
    settings::SETTINGS.with(|s| s.borrow_mut().show_changes = on);
    String::new()
}

fn cmd_timer(arg: &str) -> String {
    if arg.is_empty() {
        let on = settings::SETTINGS.with(|s| s.borrow().show_timer);
        return format!("timer: {}\n", if on { "on" } else { "off" });
    }
    let on = parse_on_off(arg);
    settings::SETTINGS.with(|s| s.borrow_mut().show_timer = on);
    String::new()
}

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

fn cmd_headers(arg: &str) -> String {
    let v = parse_on_off(arg);
    settings::SETTINGS.with(|s| s.borrow_mut().headers = v);
    String::new()
}

fn cmd_mode(arg: &str) -> String {
    let m = match arg {
        "list" => Mode::List,
        "csv" => Mode::Csv,
        "line" => Mode::Line,
        "column" => Mode::Column,
        "table" => Mode::Table,
        "markdown" => Mode::Markdown,
        "tabs" => Mode::Tabs,
        "json" => Mode::Json,
        _ => return format!("Unknown mode: {arg}\n"),
    };
    settings::SETTINGS.with(|s| s.borrow_mut().mode = m);
    String::new()
}

fn cmd_nullvalue(arg: &str) -> String {
    settings::SETTINGS.with(|s| s.borrow_mut().null_value = strip_quotes(arg));
    String::new()
}

fn cmd_separator(arg: &str) -> String {
    settings::SETTINGS.with(|s| s.borrow_mut().separator = strip_quotes(arg));
    String::new()
}

fn cmd_echo(arg: &str) -> String {
    settings::SETTINGS.with(|s| s.borrow_mut().echo = parse_on_off(arg));
    String::new()
}

fn cmd_prompt(arg: &str) -> String {
    let mut parts = arg.splitn(2, char::is_whitespace);
    let main = strip_quotes(parts.next().unwrap_or("sqlite> "));
    let cont = strip_quotes(parts.next().unwrap_or("   ...> ").trim());
    settings::SETTINGS.with(|s| {
        let mut g = s.borrow_mut();
        g.prompt_main = main;
        g.prompt_cont = cont;
    });
    String::new()
}

fn cmd_bail(arg: &str) -> String {
    settings::SETTINGS.with(|s| s.borrow_mut().bail = parse_on_off(arg));
    String::new()
}

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
