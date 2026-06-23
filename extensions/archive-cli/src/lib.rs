//! `.archive`  Phase 5 follow-up: migrated out of cli/src/dot.rs.
//!
//! SQLAR ops:
//!   --list / -t             list archive contents (default)
//!   --extract / -x          extract to --directory
//!   --create / -c           archive listed files (wipes first)
//!   --update / -u           archive listed files (incremental)
//!
//! All operations target the cli's main db via spi.execute. The
//! upstream --file FILE flag (open a separate db) needs a new
//! spi method; deferred.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use miniz_oxide::deflate::compress_to_vec_zlib;
    use miniz_oxide::inflate::decompress_to_vec_zlib;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "dotcmd-aware",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::dot_command::{
        Guest as DotCommandGuest, InvokeContext, InvokeResult,
    };
    use bindings::exports::sqlite::extension::metadata::{
        DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::cli_stdout;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_ARCHIVE: u64 = 1;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "archive-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![DotCommandSpec {
                    id: FID_ARCHIVE,
                    name: "archive".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    summary: "SQLAR (SQLite Archive) operations".into(),
                    usage: "archive [--list|--extract|--create|--update] \
                            [--directory DIR] [FILES...]".into(),
                    help: "Store / retrieve files as rows in a `sqlar` table. \
                           --list (default) prints (size, name). --extract writes \
                           each blob into --directory (default `.`). --create \
                           wipes + re-archives; --update merges. v1 omits the \
                           upstream --file flag (which uses a separate db); \
                           operations always target the cli's main db.".into(),
                    examples: alloc::vec![],
                    requires_write: false,
                    no_args: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("archive-cli: no scalar functions".into())
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Op { List, Extract, Create, Update }

    struct ArgsParse {
        op: Op,
        dir: String,
        positional: Vec<String>,
        file_unsupported: bool,
    }

    fn parse_args(arg: &str) -> ArgsParse {
        let mut op = Op::List;
        let mut dir: String = ".".into();
        let mut positional: Vec<String> = Vec::new();
        let mut file_unsupported = false;
        let mut toks = arg.split_whitespace().peekable();
        while let Some(t) = toks.next() {
            match t {
                "--list" | "-t"    => op = Op::List,
                "--extract" | "-x" => op = Op::Extract,
                "--create" | "-c"  => op = Op::Create,
                "--update" | "-u"  => op = Op::Update,
                "--file" | "-f"    => { let _ = toks.next(); file_unsupported = true; }
                "--directory" | "-C" => {
                    if let Some(d) = toks.next() { dir = d.to_string(); }
                }
                other if other.starts_with("--file=") => file_unsupported = true,
                other if other.starts_with("--directory=") => {
                    dir = other.trim_start_matches("--directory=").to_string();
                }
                other => positional.push(other.to_string()),
            }
        }
        ArgsParse { op, dir, positional, file_unsupported }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            if func_id != FID_ARCHIVE {
                return Err(SqliteError {
                    code: 1, extended_code: 1,
                    message: format!("archive-cli: unknown func id {func_id}"),
                });
            }
            Ok(dispatch(ctx.args.trim()))
        }
    }

    fn dispatch(arg: &str) -> InvokeResult {
        let p = parse_args(arg);
        if p.file_unsupported {
            return err(".archive --file ARG: separate-db operation not yet \
                wired (needs spi.open-other-db). v1 only operates on the \
                cli's main db.".into());
        }
        let needs_table = matches!(p.op, Op::Create | Op::Update);
        if needs_table {
            let sql = "CREATE TABLE IF NOT EXISTS sqlar(\
                       name TEXT PRIMARY KEY, mode INT, mtime INT, sz INT, data BLOB\
                       ) WITHOUT ROWID";
            if let Err(e) = spi::execute(sql, &[]) {
                return err(format!(".archive: create sqlar: {}", e.message));
            }
        }
        match p.op {
            Op::List    => op_list(&p.positional),
            Op::Extract => op_extract(&p.dir, &p.positional),
            Op::Create  => op_create_or_update(true, &p.positional),
            Op::Update  => op_create_or_update(false, &p.positional),
        }
    }

    fn op_list(globs: &[String]) -> InvokeResult {
        let (sql, params) = if globs.is_empty() {
            ("SELECT name, sz FROM sqlar ORDER BY name".to_string(), Vec::<SqlValue>::new())
        } else {
            let preds = globs.iter().map(|_| "name GLOB ?").collect::<Vec<_>>().join(" OR ");
            let sql = format!("SELECT name, sz FROM sqlar WHERE {preds} ORDER BY name");
            let params = globs.iter().map(|g| SqlValue::Text(g.clone())).collect();
            (sql, params)
        };
        let result = match spi::execute(&sql, &params) {
            Ok(r) => r,
            Err(e) => return err(format!(".archive --list: {}", e.message)),
        };
        let mut out = String::new();
        for row in &result.rows {
            let name = if let Some(SqlValue::Text(s)) = row.first() { s.clone() } else { continue };
            let sz   = if let Some(SqlValue::Integer(n)) = row.get(1) { *n } else { continue };
            out.push_str(&format!("{sz:>10}  {name}\n"));
        }
        if out.is_empty() {
            cli_stdout::write("(archive empty)\n");
        } else {
            cli_stdout::write(&out);
        }
        ok()
    }

    fn op_extract(dir: &str, globs: &[String]) -> InvokeResult {
        let (sql, params) = if globs.is_empty() {
            ("SELECT name, sz, data FROM sqlar".to_string(), Vec::<SqlValue>::new())
        } else {
            let preds = globs.iter().map(|_| "name GLOB ?").collect::<Vec<_>>().join(" OR ");
            let sql = format!("SELECT name, sz, data FROM sqlar WHERE {preds}");
            let params = globs.iter().map(|g| SqlValue::Text(g.clone())).collect();
            (sql, params)
        };
        let result = match spi::execute(&sql, &params) {
            Ok(r) => r,
            Err(e) => return err(format!(".archive --extract: {}", e.message)),
        };
        let mut count = 0u64;
        let mut errs = String::new();
        for row in &result.rows {
            let name = if let Some(SqlValue::Text(s)) = row.first() { s.clone() } else { continue };
            let sz   = if let Some(SqlValue::Integer(n)) = row.get(1) { *n as usize } else { continue };
            let data = match row.get(2) {
                Some(SqlValue::Blob(b)) => b.clone(),
                _ => continue,
            };
            let payload = if sz == data.len() {
                data
            } else {
                match decompress_to_vec_zlib(&data) {
                    Ok(d) => d,
                    Err(e) => {
                        errs.push_str(&format!("decompress {name}: {e:?}\n"));
                        continue;
                    }
                }
            };
            let rel = name.trim_start_matches('/');
            let target = std::path::Path::new(dir).join(rel);
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&target, &payload) {
                errs.push_str(&format!("write {}: {e}\n", target.display()));
                continue;
            }
            count += 1;
        }
        let mut out = errs;
        out.push_str(&format!("Extracted {count} file(s) to {dir}\n"));
        cli_stdout::write(&out);
        ok()
    }

    fn op_create_or_update(is_create: bool, files: &[String]) -> InvokeResult {
        if files.is_empty() {
            return err("Usage: .archive --create FILE [FILES...]".into());
        }
        if is_create {
            if let Err(e) = spi::execute("DELETE FROM sqlar", &[]) {
                return err(format!(".archive: clear sqlar: {}", e.message));
            }
        }
        let mut count = 0u64;
        let mut errs = String::new();
        for fname in files {
            let raw = match std::fs::read(fname) {
                Ok(b) => b,
                Err(e) => { errs.push_str(&format!("read {fname}: {e}\n")); continue; }
            };
            let raw_len = raw.len() as i64;
            let compressed = compress_to_vec_zlib(&raw, 6);
            let (data, sz) = if compressed.len() < raw.len() {
                (compressed, raw_len)
            } else {
                (raw, raw_len)
            };
            let params = alloc::vec![
                SqlValue::Text(fname.clone()),
                SqlValue::Integer(0o100644),
                SqlValue::Integer(0),
                SqlValue::Integer(sz),
                SqlValue::Blob(data),
            ];
            let sql = "INSERT OR REPLACE INTO sqlar(name, mode, mtime, sz, data) \
                       VALUES (?, ?, ?, ?, ?)";
            if let Err(e) = spi::execute(sql, &params) {
                errs.push_str(&format!("insert {fname}: {}\n", e.message));
                continue;
            }
            count += 1;
        }
        let mut out = errs;
        out.push_str(&format!("{} {count} file(s) into sqlar\n",
            if is_create { "Archived" } else { "Updated" }));
        cli_stdout::write(&out);
        ok()
    }

    fn ok() -> InvokeResult {
        InvokeResult {
            text: String::new(),
            state_deltas: alloc::vec![],
            ok: true,
            exit_code: 0,
        }
    }

    fn err(message: String) -> InvokeResult {
        InvokeResult {
            text: format!("{message}\n"),
            state_deltas: alloc::vec![],
            ok: false,
            exit_code: 1,
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
