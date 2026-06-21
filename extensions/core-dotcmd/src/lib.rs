//! Seed port of `cli/src/dot.rs` built-in dot commands as a
//! dotcmd-aware wasm extension. v1 ports the simplest +
//! highest-value commands:
//!
//!   .version          show sqlite + sqlink version
//!   .help [NAME]      list commands / show one
//!   .tables [PATTERN] list tables in the schema
//!   .schema [TABLE]   show CREATE statements
//!   .databases        list attached databases
//!
//! Each command pulls data via `spi.execute` so the wasm
//! boundary is the only data path. cli-stdout writes are
//! buffered into invoke-result.text  the host streams it
//! straight to stdout.
//!
//! Subsequent batches port the remaining ~30 commands and then
//! `cli/src/dot.rs` can be deleted; the dispatcher's "built-in
//! match" path becomes a single registry walk.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

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
        DotCommandExample, DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::cli_stdout;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_VERSION:   u64 = 1;
    const FID_HELP:      u64 = 2;
    const FID_TABLES:    u64 = 3;
    const FID_SCHEMA:    u64 = 4;
    const FID_DATABASES: u64 = 5;
    const FID_INDEXES:    u64 = 6;
    const FID_DBINFO:     u64 = 7;
    const FID_FULLSCHEMA: u64 = 8;
    const FID_LINT:       u64 = 9;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let spec = |id, name: &str, summary: &str, usage: &str, help: &str| DotCommandSpec {
                id,
                name: name.into(),
                version: env!("CARGO_PKG_VERSION").into(),
                summary: summary.into(),
                usage: usage.into(),
                help: help.into(),
                examples: alloc::vec![],
                requires_write: false,
                no_args: false,
            };
            Manifest {
                name: "core-dotcmd".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![
                    spec(FID_VERSION, "version",
                         "Show SQLite + sqlink version",
                         "version",
                         "Prints the bundled SQLite version followed by the sqlink CLI version."),
                    spec(FID_HELP, "help",
                         "Show command help",
                         "help [NAME]",
                         "Without args, lists every known dot command. With NAME, shows that one's detailed help."),
                    spec(FID_TABLES, "tables",
                         "List tables in the schema",
                         "tables [PATTERN]",
                         "Lists base + view names. PATTERN is a LIKE pattern (use % for wildcard)."),
                    spec(FID_SCHEMA, "schema",
                         "Show CREATE statements",
                         "schema [TABLE]",
                         "Dumps the CREATE TABLE / INDEX / VIEW / TRIGGER statements. With TABLE, restricts to that one."),
                    spec(FID_DATABASES, "databases",
                         "List attached databases",
                         "databases",
                         "Equivalent to PRAGMA database_list  shows seq, name, file path."),
                    spec(FID_INDEXES, "indexes",
                         "List indexes (optionally per table)",
                         "indexes [TABLE]",
                         "Without TABLE, lists every index in the schema. With TABLE, restricts to indexes on that table."),
                    spec(FID_DBINFO, "dbinfo",
                         "Print db info from PRAGMAs",
                         "dbinfo",
                         "Walks page_size, page_count, freelist_count, encoding, user_version, application_id, journal_mode, synchronous, auto_vacuum  one PRAGMA per row."),
                    spec(FID_FULLSCHEMA, "fullschema",
                         "Show schema + ANALYZE data",
                         "fullschema",
                         "Like .schema but also dumps sqlite_stat1 / sqlite_stat4 rows when ANALYZE has run."),
                    spec(FID_LINT, "lint",
                         "Report schema issues",
                         "lint [SUBCOMMAND]",
                         "Today: `lint fkey-indexes` (default) flags foreign keys with no backing index."),
                ],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("core-dotcmd: no scalar functions".to_string())
        }
    }

    /// Execute a SQL statement that returns at most one TEXT
    /// column. Each row is collected as TEXT; non-TEXT cells get
    /// stringified.
    fn query_text_col(sql: &str, params: &[SqlValue]) -> Result<Vec<String>, SqliteError> {
        let result = spi::execute(sql, params)?;
        let mut out = Vec::with_capacity(result.rows.len());
        for row in &result.rows {
            if let Some(v) = row.first() {
                out.push(match v {
                    SqlValue::Null => String::new(),
                    SqlValue::Integer(n) => n.to_string(),
                    SqlValue::Real(r) => r.to_string(),
                    SqlValue::Text(s) => s.clone(),
                    SqlValue::Blob(b) => format!("<blob:{} bytes>", b.len()),
                });
            }
        }
        Ok(out)
    }

    fn cmd_version() -> InvokeResult {
        // sqlite_version() is a SQL function; spi will execute it.
        let version = query_text_col("SELECT sqlite_version()", &[])
            .map(|v| v.into_iter().next().unwrap_or_default())
            .unwrap_or_else(|_| "?".to_string());
        let body = format!(
            "SQLite {}\nsqlink core-dotcmd {}\n",
            version,
            env!("CARGO_PKG_VERSION"),
        );
        cli_stdout::write(&body);
        ok()
    }

    fn cmd_help(arg: &str) -> InvokeResult {
        let entries: &[(&str, &str)] = &[
            ("version",   "Show SQLite + sqlink version"),
            ("help",      "Show command help"),
            ("tables",    "List tables in the schema"),
            ("schema",    "Show CREATE statements"),
            ("databases", "List attached databases"),
        ];
        if arg.is_empty() {
            cli_stdout::write("Available commands:\n");
            for (n, s) in entries {
                cli_stdout::write(&format!("  .{:<14}  {}\n", n, s));
            }
        } else {
            let lc = arg.to_lowercase();
            match entries.iter().find(|(n, _)| *n == lc) {
                Some((n, s)) => cli_stdout::write(&format!(".{}  {}\n", n, s)),
                None => cli_stdout::write(&format!("Unknown command: .{}\n", arg)),
            }
        }
        ok()
    }

    fn cmd_tables(arg: &str) -> InvokeResult {
        let pattern = if arg.is_empty() { "%".to_string() } else { arg.to_string() };
        let sql = "SELECT name FROM sqlite_master \
                   WHERE type IN ('table','view') AND name LIKE ? \
                   ORDER BY name";
        let rows = match query_text_col(sql, &[SqlValue::Text(pattern)]) {
            Ok(v) => v,
            Err(e) => return err(format!("tables: {}", e.message)),
        };
        for r in &rows {
            cli_stdout::write(r);
            cli_stdout::write("\n");
        }
        if rows.is_empty() {
            cli_stdout::write("(no matching tables)\n");
        }
        ok()
    }

    fn cmd_schema(arg: &str) -> InvokeResult {
        let (sql, params): (String, Vec<SqlValue>) = if arg.is_empty() {
            ("SELECT sql FROM sqlite_master \
              WHERE sql IS NOT NULL \
              ORDER BY type DESC, name".to_string(), Vec::new())
        } else {
            (
                "SELECT sql FROM sqlite_master \
                 WHERE sql IS NOT NULL AND name = ? \
                 ORDER BY type DESC".to_string(),
                alloc::vec![SqlValue::Text(arg.to_string())],
            )
        };
        let rows = match query_text_col(&sql, &params) {
            Ok(v) => v,
            Err(e) => return err(format!("schema: {}", e.message)),
        };
        for r in &rows {
            cli_stdout::write(r);
            cli_stdout::write(";\n");
        }
        if rows.is_empty() {
            cli_stdout::write("(empty)\n");
        }
        ok()
    }

    fn cmd_indexes(arg: &str) -> InvokeResult {
        let (sql, params): (String, Vec<SqlValue>) = if arg.is_empty() {
            (
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' \
                 ORDER BY name".to_string(),
                Vec::new(),
            )
        } else {
            (
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' AND tbl_name = ? \
                 ORDER BY name".to_string(),
                alloc::vec![SqlValue::Text(arg.to_string())],
            )
        };
        let rows = match query_text_col(&sql, &params) {
            Ok(v) => v,
            Err(e) => return err(format!("indexes: {}", e.message)),
        };
        for r in &rows {
            cli_stdout::write(r);
            cli_stdout::write("\n");
        }
        if rows.is_empty() {
            cli_stdout::write("(no matching indexes)\n");
        }
        ok()
    }

    fn cmd_dbinfo() -> InvokeResult {
        // Same pragma probes the built-in `.dbinfo` uses; each
        // ships at most one row, formatted as `<label>  <value>`.
        let probes: &[(&str, &str)] = &[
            ("page size",      "PRAGMA page_size"),
            ("page count",     "PRAGMA page_count"),
            ("freelist count", "PRAGMA freelist_count"),
            ("encoding",       "PRAGMA encoding"),
            ("user version",   "PRAGMA user_version"),
            ("application id", "PRAGMA application_id"),
            ("journal mode",   "PRAGMA journal_mode"),
            ("synchronous",    "PRAGMA synchronous"),
            ("auto vacuum",    "PRAGMA auto_vacuum"),
        ];
        for (label, sql) in probes {
            if let Ok(rows) = query_text_col(sql, &[]) {
                if let Some(v) = rows.into_iter().next() {
                    cli_stdout::write(&format!("{:<18}{}\n", label, v));
                }
            }
        }
        ok()
    }

    fn cmd_fullschema() -> InvokeResult {
        // Schema (every CREATE that has SQL).
        let create = match query_text_col(
            "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid",
            &[],
        ) {
            Ok(v) => v,
            Err(e) => return err(format!("fullschema: {}", e.message)),
        };
        for sql in &create {
            cli_stdout::write(sql);
            cli_stdout::write(";\n");
        }
        // ANALYZE data.
        if let Ok(stat) = query_text_col(
            "SELECT 'INSERT INTO sqlite_stat1 VALUES(' \
                  || quote(tbl) || ',' || quote(idx) || ',' || quote(stat) || ');' \
             FROM sqlite_stat1",
            &[],
        ) {
            for s in &stat {
                cli_stdout::write(s);
                cli_stdout::write("\n");
            }
        }
        ok()
    }

    fn cmd_lint(arg: &str) -> InvokeResult {
        let sub = arg.trim();
        if !sub.is_empty() && sub != "fkey-indexes" {
            return err(format!("lint: only `fkey-indexes` supported (got {sub:?})"));
        }
        // Walk every user table; for each FK, check whether any
        // index leads with the FK column. Pure-SQL approach using
        // pragma_foreign_key_list + pragma_index_info.
        let tables = match query_text_col(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            &[],
        ) {
            Ok(v) => v,
            Err(e) => return err(format!("lint: {}", e.message)),
        };
        let mut any = false;
        for tbl in &tables {
            let fk_sql = format!("PRAGMA foreign_key_list({})", quote_ident(tbl));
            let result = match spi::execute(&fk_sql, &[]) {
                Ok(r) => r,
                Err(_) => continue,
            };
            for row in &result.rows {
                let id = row.first().and_then(int_of).unwrap_or(0);
                let from = row.get(3).and_then(text_of).unwrap_or_default();
                let to_tbl = row.get(2).and_then(text_of).unwrap_or_default();
                let to_col = row.get(4).and_then(text_of).unwrap_or_default();
                if has_leading_index(tbl, &from) {
                    continue;
                }
                any = true;
                cli_stdout::write(&format!(
                    "Missing index for FK {tbl}({from}) -> {to_tbl}({to_col}); \
                     suggest: CREATE INDEX idx_{tbl}_{from} ON {tbl}({from}); -- fk id {id}\n"
                ));
            }
        }
        if !any {
            cli_stdout::write("(no foreign-key index gaps)\n");
        }
        ok()
    }

    /// Lightweight ident quoting  good enough for table names
    /// returned from sqlite_master where backticks/specials are
    /// rare. For full robustness, the cli's escape() helper would
    /// be ported too; this is fit-for-purpose.
    fn quote_ident(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }

    fn text_of(v: &SqlValue) -> Option<String> {
        match v {
            SqlValue::Text(s) => Some(s.clone()),
            SqlValue::Integer(n) => Some(n.to_string()),
            SqlValue::Real(r) => Some(r.to_string()),
            _ => None,
        }
    }
    fn int_of(v: &SqlValue) -> Option<i64> {
        match v {
            SqlValue::Integer(n) => Some(*n),
            SqlValue::Real(r) => Some(*r as i64),
            _ => None,
        }
    }

    /// True if any index on `table` has `col` as its first column.
    fn has_leading_index(table: &str, col: &str) -> bool {
        let q = "SELECT name FROM sqlite_master \
                 WHERE type='index' AND tbl_name=?";
        let result = match spi::execute(q, &[SqlValue::Text(table.to_string())]) {
            Ok(r) => r,
            Err(_) => return false,
        };
        for row in &result.rows {
            let Some(idx) = row.first().and_then(text_of) else { continue };
            let info_sql = format!("PRAGMA index_info({})", quote_ident(&idx));
            let info = match spi::execute(&info_sql, &[]) {
                Ok(r) => r,
                Err(_) => continue,
            };
            // index_info columns: seqno, cid, name. seqno = 0 means
            // first column of the index.
            for irow in &info.rows {
                let seq = irow.first().and_then(int_of).unwrap_or(-1);
                let name = irow.get(2).and_then(text_of).unwrap_or_default();
                if seq == 0 && name == col {
                    return true;
                }
            }
        }
        false
    }

    fn cmd_databases() -> InvokeResult {
        let result = match spi::execute("PRAGMA database_list", &[]) {
            Ok(r) => r,
            Err(e) => return err(format!("databases: {}", e.message)),
        };
        // PRAGMA database_list returns (seq, name, file).
        for row in &result.rows {
            let seq = row.first().map(format_val).unwrap_or_default();
            let name = row.get(1).map(format_val).unwrap_or_default();
            let file = row.get(2).map(format_val).unwrap_or_default();
            cli_stdout::write(&format!("{:<4}  {:<8}  {}\n", seq, name, file));
        }
        ok()
    }

    fn format_val(v: &SqlValue) -> String {
        match v {
            SqlValue::Null => String::new(),
            SqlValue::Integer(n) => n.to_string(),
            SqlValue::Real(r) => r.to_string(),
            SqlValue::Text(s) => s.clone(),
            SqlValue::Blob(b) => format!("<{}b>", b.len()),
        }
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
            text: format!("{}\n", message),
            state_deltas: alloc::vec![],
            ok: false,
            exit_code: 1,
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(
            func_id: u64,
            ctx: InvokeContext,
        ) -> Result<InvokeResult, SqliteError> {
            let arg = ctx.args.trim();
            Ok(match func_id {
                FID_VERSION   => cmd_version(),
                FID_HELP      => cmd_help(arg),
                FID_TABLES    => cmd_tables(arg),
                FID_SCHEMA    => cmd_schema(arg),
                FID_DATABASES => cmd_databases(),
                FID_INDEXES   => cmd_indexes(arg),
                FID_DBINFO    => cmd_dbinfo(),
                FID_FULLSCHEMA => cmd_fullschema(),
                FID_LINT      => cmd_lint(arg),
                _ => return Err(SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("core-dotcmd: unknown func id {func_id}"),
                }),
            })
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
