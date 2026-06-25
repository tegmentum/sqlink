//! sqlite-utils maintenance dot commands.
//!
//! PLAN-sqlite-utils-port.md Stage 4. 8 commands:
//!   .vacuum            VACUUM (reports bytes freed)
//!   .analyze [TABLE]   ANALYZE (whole db or one table)
//!   .optimize          PRAGMA optimize + per-fts optimize
//!   .enable_wal        PRAGMA journal_mode = WAL
//!   .disable_wal       PRAGMA journal_mode = DELETE
//!   .enable_counts     _counts table + per-user-table triggers
//!   .reset_counts      recompute _counts rows
//!   .create_database   sqlite-utils parity stub

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
        DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::cli_state;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_VACUUM:          u64 = 1;
    const FID_ANALYZE:         u64 = 2;
    const FID_OPTIMIZE:        u64 = 3;
    const FID_ENABLE_WAL:      u64 = 4;
    const FID_DISABLE_WAL:     u64 = 5;
    const FID_ENABLE_COUNTS:   u64 = 6;
    const FID_RESET_COUNTS:    u64 = 7;
    const FID_CREATE_DATABASE: u64 = 8;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let cmd = |id, name: &str, summary: &str, usage: &str, help: &str, requires_write: bool, no_args: bool| {
                DotCommandSpec {
                    id,
                    name: name.into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    summary: summary.into(),
                    usage: usage.into(),
                    help: help.into(),
                    examples: alloc::vec![],
                    requires_write,
                    no_args,
                }
            };
            Manifest {
                name: "sqlite-utils-maint".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![
                    cmd(
                        FID_VACUUM,
                        "vacuum",
                        "Rebuild the database file to reclaim space",
                        "vacuum",
                        "Runs VACUUM. Must NOT be inside an open transaction.\n\
                         Reports bytes freed (page_count * page_size before and after).",
                        true,
                        true,
                    ),
                    cmd(
                        FID_ANALYZE,
                        "analyze",
                        "Update sqlite_stat tables (whole db or one table)",
                        "analyze [TABLE]",
                        "Runs ANALYZE [TABLE]. With no arg, analyzes every table.",
                        true,
                        false,
                    ),
                    cmd(
                        FID_OPTIMIZE,
                        "optimize",
                        "PRAGMA optimize + optimize each *_fts virtual table",
                        "optimize",
                        "Runs PRAGMA optimize and (for every <X>_fts table) \
                         INSERT INTO <X>_fts(<X>_fts) VALUES('optimize').",
                        true,
                        true,
                    ),
                    cmd(
                        FID_ENABLE_WAL,
                        "enable_wal",
                        "PRAGMA journal_mode = WAL",
                        "enable_wal",
                        "Switches the journal mode to WAL. Prints the resulting mode.",
                        true,
                        true,
                    ),
                    cmd(
                        FID_DISABLE_WAL,
                        "disable_wal",
                        "PRAGMA journal_mode = DELETE",
                        "disable_wal",
                        "Switches the journal mode back to DELETE (the sqlite default). \
                         Prints the resulting mode.",
                        true,
                        true,
                    ),
                    cmd(
                        FID_ENABLE_COUNTS,
                        "enable_counts",
                        "Maintain a _counts(table, count) table via triggers",
                        "enable_counts",
                        "Creates _counts(table TEXT PRIMARY KEY, count INTEGER), seeds it \
                         from current row counts, and installs AFTER INSERT/DELETE triggers \
                         on every user table so _counts stays in sync.",
                        true,
                        true,
                    ),
                    cmd(
                        FID_RESET_COUNTS,
                        "reset_counts",
                        "Recompute every row of the _counts table",
                        "reset_counts",
                        "No-op when _counts does not exist; otherwise SELECT count(*) for \
                         every user table and INSERT OR REPLACE into _counts.",
                        true,
                        true,
                    ),
                    cmd(
                        FID_CREATE_DATABASE,
                        "create_database",
                        "Confirm the current db path (parity with sqlite-utils create-database)",
                        "create_database",
                        "SQLink's `--db PATH` already opens-or-creates the database file. \
                         This command prints the active path for parity with sqlite-utils.",
                        false,
                        true,
                    ),
                ],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("sqlite-utils-maint: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            let arg = ctx.args.trim();
            Ok(match func_id {
                FID_VACUUM          => cmd_vacuum(),
                FID_ANALYZE         => cmd_analyze(arg),
                FID_OPTIMIZE        => cmd_optimize(),
                FID_ENABLE_WAL      => cmd_journal_mode("WAL"),
                FID_DISABLE_WAL     => cmd_journal_mode("DELETE"),
                FID_ENABLE_COUNTS   => cmd_enable_counts(),
                FID_RESET_COUNTS    => cmd_reset_counts(),
                FID_CREATE_DATABASE => cmd_create_database(),
                other => return Err(SqliteError {
                    code: 1, extended_code: 1,
                    message: format!("sqlite-utils-maint: unknown func id {other}"),
                }),
            })
        }
    }

    fn cmd_vacuum() -> InvokeResult {
        let before = db_size_bytes();
        match spi::execute_batch("VACUUM") {
            Ok(_) => {}
            Err(e) => return err(format!("VACUUM: {} (code {})", e.message, e.code)),
        }
        let after = db_size_bytes();
        match (before, after) {
            (Some(b), Some(a)) => {
                let delta = b as i64 - a as i64;
                if delta > 0 {
                    text(format!("VACUUM ok ({delta} bytes freed: {b}  {a})\n"))
                } else if delta < 0 {
                    text(format!("VACUUM ok ({} bytes added: {b}  {a})\n", -delta))
                } else {
                    text(format!("VACUUM ok (no change: {b} bytes)\n"))
                }
            }
            _ => text("VACUUM ok\n".into()),
        }
    }

    fn cmd_analyze(arg: &str) -> InvokeResult {
        let sql = if arg.is_empty() {
            "ANALYZE".to_string()
        } else {
            format!("ANALYZE {}", quote_ident(arg))
        };
        match spi::execute_batch(&sql) {
            Ok(_) => {
                let target = if arg.is_empty() { "<all tables>" } else { arg };
                text(format!("ANALYZE {target} ok\n"))
            }
            Err(e) => err(format!("ANALYZE: {} (code {})", e.message, e.code)),
        }
    }

    fn cmd_optimize() -> InvokeResult {
        if let Err(e) = spi::execute_batch("PRAGMA optimize") {
            return err(format!("PRAGMA optimize: {} (code {})", e.message, e.code));
        }
        // Walk fts5 tables (sqlite_master.sql LIKE '%USING fts5%') and
        // run the fts optimize command on each.
        let rows = match spi::execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND sql LIKE '%fts5%' \
             AND name NOT LIKE '%_data' AND name NOT LIKE '%_idx' \
             AND name NOT LIKE '%_content' AND name NOT LIKE '%_docsize' \
             AND name NOT LIKE '%_config'",
            &[],
        ) {
            Ok(r) => r.rows,
            Err(e) => return err(format!("fts table scan: {} (code {})", e.message, e.code)),
        };
        let mut fts_count = 0u64;
        for row in rows {
            if let Some(SqlValue::Text(name)) = row.into_iter().next() {
                let sql = format!(
                    "INSERT INTO {0}({0}) VALUES('optimize')",
                    quote_ident(&name)
                );
                if let Err(e) = spi::execute_batch(&sql) {
                    return err(format!(
                        "fts optimize {name}: {} (code {})",
                        e.message, e.code
                    ));
                }
                fts_count += 1;
            }
        }
        text(format!("optimize ok (pragma + {fts_count} fts table(s))\n"))
    }

    fn cmd_journal_mode(mode: &str) -> InvokeResult {
        let sql = format!("PRAGMA journal_mode = {mode}");
        match spi::execute(&sql, &[]) {
            Ok(qr) => {
                let actual = qr
                    .rows
                    .into_iter()
                    .next()
                    .and_then(|r| r.into_iter().next())
                    .map(|v| match v {
                        SqlValue::Text(s) => s,
                        other => format!("{other:?}"),
                    })
                    .unwrap_or_else(|| "<unknown>".into());
                text(format!("journal_mode = {actual}\n"))
            }
            Err(e) => err(format!("PRAGMA journal_mode={mode}: {} (code {})", e.message, e.code)),
        }
    }

    fn cmd_enable_counts() -> InvokeResult {
        let mut out = String::new();
        if let Err(e) = spi::execute_batch(
            "CREATE TABLE IF NOT EXISTS _counts (\
                 \"table\" TEXT PRIMARY KEY,\
                 count INTEGER NOT NULL DEFAULT 0\
             )",
        ) {
            return err(format!("create _counts: {} (code {})", e.message, e.code));
        }
        let tables = match user_tables() {
            Ok(t) => t,
            Err(e) => return err(e),
        };
        for t in &tables {
            let seed = format!(
                "INSERT OR REPLACE INTO _counts(\"table\", count) \
                 SELECT '{t}', count(*) FROM {qt}",
                t = sql_string_lit(t),
                qt = quote_ident(t),
            );
            if let Err(e) = spi::execute_batch(&seed) {
                return err(format!("seed _counts for {t}: {} (code {})", e.message, e.code));
            }
            let trig_i = format!(
                "CREATE TRIGGER IF NOT EXISTS _counts_{safe}_i AFTER INSERT ON {qt} BEGIN \
                   INSERT OR REPLACE INTO _counts(\"table\", count) \
                   VALUES('{lit}', coalesce((SELECT count FROM _counts WHERE \"table\"='{lit}'), 0) + 1); \
                 END",
                safe = safe_ident(t),
                qt = quote_ident(t),
                lit = sql_string_lit(t),
            );
            if let Err(e) = spi::execute_batch(&trig_i) {
                return err(format!("create insert trigger for {t}: {} (code {})", e.message, e.code));
            }
            let trig_d = format!(
                "CREATE TRIGGER IF NOT EXISTS _counts_{safe}_d AFTER DELETE ON {qt} BEGIN \
                   INSERT OR REPLACE INTO _counts(\"table\", count) \
                   VALUES('{lit}', coalesce((SELECT count FROM _counts WHERE \"table\"='{lit}'), 0) - 1); \
                 END",
                safe = safe_ident(t),
                qt = quote_ident(t),
                lit = sql_string_lit(t),
            );
            if let Err(e) = spi::execute_batch(&trig_d) {
                return err(format!("create delete trigger for {t}: {} (code {})", e.message, e.code));
            }
            out.push_str(&format!("tracking {t}\n"));
        }
        if tables.is_empty() {
            out.push_str("no user tables found\n");
        }
        text(out)
    }

    fn cmd_reset_counts() -> InvokeResult {
        // Existence check.
        let exists = match spi::execute_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='_counts'",
            &[],
        ) {
            Ok(SqlValue::Integer(n)) => n > 0,
            Ok(_) => false,
            Err(e) => return err(format!("check _counts: {} (code {})", e.message, e.code)),
        };
        if !exists {
            return text("_counts does not exist; run .enable_counts first\n".into());
        }
        let tables = match user_tables() {
            Ok(t) => t,
            Err(e) => return err(e),
        };
        // Wipe and recompute.
        if let Err(e) = spi::execute_batch("DELETE FROM _counts") {
            return err(format!("clear _counts: {} (code {})", e.message, e.code));
        }
        let mut out = String::new();
        for t in &tables {
            let sql = format!(
                "INSERT OR REPLACE INTO _counts(\"table\", count) \
                 SELECT '{lit}', count(*) FROM {qt}",
                lit = sql_string_lit(t),
                qt = quote_ident(t),
            );
            if let Err(e) = spi::execute_batch(&sql) {
                return err(format!("recompute {t}: {} (code {})", e.message, e.code));
            }
            out.push_str(&format!("recomputed {t}\n"));
        }
        if tables.is_empty() {
            out.push_str("no user tables found\n");
        }
        text(out)
    }

    fn cmd_create_database() -> InvokeResult {
        let path = cli_state::get_text("db/path");
        // No-op SQL to confirm the connection is live.
        if let Err(e) = spi::execute_scalar("SELECT 1", &[]) {
            return err(format!("verify connection: {} (code {})", e.message, e.code));
        }
        let body = if path.is_empty() {
            "database is in-memory (no --db PATH)\n".to_string()
        } else {
            format!(
                "database path: {path}\n(sqlink's --db PATH opens-or-creates; nothing to do)\n"
            )
        };
        text(body)
    }

    // --- helpers ---

    fn db_size_bytes() -> Option<u64> {
        let page_count = match spi::execute_scalar("PRAGMA page_count", &[]) {
            Ok(SqlValue::Integer(n)) if n >= 0 => n as u64,
            _ => return None,
        };
        let page_size = match spi::execute_scalar("PRAGMA page_size", &[]) {
            Ok(SqlValue::Integer(n)) if n >= 0 => n as u64,
            _ => return None,
        };
        Some(page_count * page_size)
    }

    fn user_tables() -> Result<Vec<String>, String> {
        // Exclude:
        //   * sqlite_* internal tables
        //   * _-prefixed bookkeeping tables (e.g. _counts itself)
        //   * virtual tables (their sql starts with CREATE VIRTUAL TABLE
        //     and sqlite forbids triggers on them)
        //   * virtual-table shadow tables (e.g. fts5's
        //     <vtab>_data / _idx / _docsize / _config). Despite being
        //     created by the vtab module, sqlite STILL writes a fake
        //     `CREATE TABLE '<name>_kind'(...)` row to sqlite_master.
        //     Detect them via the correlated EXISTS clause: a row is
        //     a shadow iff there's a CREATE VIRTUAL TABLE row whose
        //     name is a prefix of this row's name.
        let qr = spi::execute(
            "SELECT t1.name FROM sqlite_master t1 \
             WHERE t1.type='table' \
               AND t1.name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
               AND t1.name NOT LIKE '\\_%' ESCAPE '\\' \
               AND (t1.sql IS NULL OR t1.sql NOT LIKE 'CREATE VIRTUAL%') \
               AND NOT EXISTS ( \
                   SELECT 1 FROM sqlite_master t2 \
                   WHERE t2.type='table' \
                     AND t2.sql LIKE 'CREATE VIRTUAL%' \
                     AND t1.name LIKE t2.name || '\\_%' ESCAPE '\\' \
               ) \
             ORDER BY t1.name",
            &[],
        )
        .map_err(|e| format!("list tables: {} (code {})", e.message, e.code))?;
        let mut out = Vec::with_capacity(qr.rows.len());
        for row in qr.rows {
            if let Some(SqlValue::Text(n)) = row.into_iter().next() {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// Double-quote an identifier and escape any embedded `"`.
    fn quote_ident(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            if c == '"' { out.push('"'); }
            out.push(c);
        }
        out.push('"');
        out
    }

    /// Escape a string for use inside a SQL `'...'` literal.
    fn sql_string_lit(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if c == '\'' { out.push('\''); }
            out.push(c);
        }
        out
    }

    /// Sanitize an identifier for use as a SQL trigger-name suffix.
    /// Replaces non-alphanumeric with `_`.
    fn safe_ident(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if c.is_ascii_alphanumeric() { out.push(c); } else { out.push('_'); }
        }
        out
    }

    fn text(body: String) -> InvokeResult {
        InvokeResult { text: body, state_deltas: alloc::vec![], ok: true, exit_code: 0 }
    }

    fn err(message: String) -> InvokeResult {
        InvokeResult {
            text: format!("Error: {message}\n"),
            state_deltas: alloc::vec![],
            ok: false,
            exit_code: 1,
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
