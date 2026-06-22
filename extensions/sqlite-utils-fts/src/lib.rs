//! FTS5 helpers ported from `sqlite-utils`:
//!   .enable_fts TABLE COL ... [--create-triggers] [--tokenize T]
//!   .disable_fts TABLE
//!   .rebuild_fts [TABLE]
//!   .populate_fts TABLE COL ...
//!   .search TABLE QUERY [--limit N] [--columns col1,col2,...]
//!
//! Stage 3 of PLAN-sqlite-utils-port.md.

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
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_ENABLE: u64 = 1;
    const FID_DISABLE: u64 = 2;
    const FID_REBUILD: u64 = 3;
    const FID_POPULATE: u64 = 4;
    const FID_SEARCH: u64 = 5;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "sqlite-utils-fts".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![
                    spec(FID_ENABLE, "enable_fts",
                         "Create an FTS5 external-content table indexing COL(s) of TABLE",
                         "enable_fts TABLE COL [COL ...] [--create-triggers] [--tokenize T]",
                         "Creates TABLE_fts USING fts5(... content=TABLE, content_rowid=rowid). \
                          --create-triggers installs AFTER INSERT/DELETE/UPDATE triggers to keep \
                          the index in sync. --tokenize T sets a custom tokenizer (e.g. porter, \
                          'porter unicode61'). Populates the index immediately."),
                    spec(FID_DISABLE, "disable_fts",
                         "Drop a previously-created FTS5 table + its triggers",
                         "disable_fts TABLE",
                         "Drops TABLE_fts and the conventional TABLE_ai/TABLE_ad/TABLE_au triggers."),
                    spec(FID_REBUILD, "rebuild_fts",
                         "Rebuild one or all FTS5 tables from their content tables",
                         "rebuild_fts [TABLE]",
                         "INSERT INTO TABLE_fts(TABLE_fts) VALUES('rebuild'). With no TABLE, \
                          rebuilds every *_fts virtual table found in sqlite_master."),
                    spec(FID_POPULATE, "populate_fts",
                         "Bulk-insert rowids + columns from a content table into its FTS5 index",
                         "populate_fts TABLE COL [COL ...]",
                         "INSERT INTO TABLE_fts(rowid, col1, ...) SELECT rowid, col1, ... FROM TABLE. \
                          For external-content FTS5 tables `.rebuild_fts` is usually equivalent."),
                    spec(FID_SEARCH, "search",
                         "Run a FTS5 MATCH query against TABLE",
                         "search TABLE QUERY [--limit N] [--columns col1,col2,...]",
                         "Joins TABLE_fts back to TABLE via rowid, returns matching rows ordered \
                          by FTS5 rank. Default LIMIT 20. --columns selects which source-table \
                          columns to render."),
                ],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("sqlite-utils-fts: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            Ok(match func_id {
                FID_ENABLE   => cmd_enable(ctx.args.trim()),
                FID_DISABLE  => cmd_disable(ctx.args.trim()),
                FID_REBUILD  => cmd_rebuild(ctx.args.trim()),
                FID_POPULATE => cmd_populate(ctx.args.trim()),
                FID_SEARCH   => cmd_search(ctx.args.trim()),
                _ => return Err(SqliteError {
                    code: 1, extended_code: 1,
                    message: format!("sqlite-utils-fts: unknown func id {func_id}"),
                }),
            })
        }
    }

    // ---- arg parsing ----

    /// Strip `--flag VALUE` and `--flag=VALUE` from a positional
    /// arg vector. Returns (positional, value-for-`flag` or None).
    /// Bool flags are handled separately.
    fn split_flag_kv(args: &mut Vec<String>, flag: &str) -> Option<String> {
        let mut i = 0;
        while i < args.len() {
            let cur = args[i].clone();
            if cur == flag {
                if i + 1 < args.len() {
                    let v = args.remove(i + 1);
                    args.remove(i);
                    return Some(v);
                }
                // dangling flag  drop it
                args.remove(i);
                return None;
            }
            if let Some(eq) = cur.strip_prefix(&format!("{flag}=")) {
                let v = eq.to_string();
                args.remove(i);
                return Some(v);
            }
            i += 1;
        }
        None
    }

    /// Strip a bool flag like `--create-triggers` from args. Returns true if found.
    fn split_flag_bool(args: &mut Vec<String>, flag: &str) -> bool {
        if let Some(idx) = args.iter().position(|a| a == flag) {
            args.remove(idx);
            true
        } else {
            false
        }
    }

    fn split_args(arg: &str) -> Vec<String> {
        arg.split_whitespace().map(|s| s.to_string()).collect()
    }

    // ---- identifier quoting ----
    //
    // We splice user-supplied table/column names into SQL. Quote
    // them with double quotes per the SQLite grammar; embedded
    // double quotes escape as "".

    fn q(name: &str) -> String {
        let mut s = String::with_capacity(name.len() + 2);
        s.push('"');
        for c in name.chars() {
            if c == '"' {
                s.push_str("\"\"");
            } else {
                s.push(c);
            }
        }
        s.push('"');
        s
    }

    /// SQL-quote a string literal (single quotes, doubled to escape).
    fn qs(value: &str) -> String {
        let mut s = String::with_capacity(value.len() + 2);
        s.push('\'');
        for c in value.chars() {
            if c == '\'' {
                s.push_str("''");
            } else {
                s.push(c);
            }
        }
        s.push('\'');
        s
    }

    // ---- shared helpers ----

    fn list_fts_tables() -> Result<Vec<String>, String> {
        // FTS5 virtual tables have sql starting with `CREATE VIRTUAL
        // TABLE ... USING fts5`. Filter by suffix to match the
        // sqlite-utils convention of <T>_fts naming.
        let r = spi::execute(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND sql LIKE 'CREATE VIRTUAL TABLE%USING fts5%' \
             ORDER BY name",
            &[],
        ).map_err(|e| e.message)?;
        Ok(r.rows.into_iter().filter_map(|row| {
            row.into_iter().next().and_then(|v| match v {
                SqlValue::Text(s) => Some(s),
                _ => None,
            })
        }).collect())
    }

    // ---- commands ----

    fn cmd_enable(arg: &str) -> InvokeResult {
        let mut args = split_args(arg);
        let create_triggers = split_flag_bool(&mut args, "--create-triggers");
        let tokenize = split_flag_kv(&mut args, "--tokenize");
        if args.len() < 2 {
            return err(".enable_fts TABLE COL [COL ...] [--create-triggers] [--tokenize T]".into());
        }
        let table = args[0].clone();
        let cols: Vec<String> = args[1..].to_vec();
        let fts = format!("{table}_fts");

        // Build CREATE VIRTUAL TABLE statement.
        let mut create = format!("CREATE VIRTUAL TABLE {} USING fts5(", q(&fts));
        for (i, c) in cols.iter().enumerate() {
            if i > 0 { create.push_str(", "); }
            create.push_str(&q(c));
        }
        create.push_str(", content=");
        create.push_str(&q(&table));
        create.push_str(", content_rowid='rowid'");
        if let Some(tok) = &tokenize {
            create.push_str(", tokenize=");
            create.push_str(&qs(tok));
        }
        create.push(')');

        // Triggers (sqlite-utils external-content convention).
        let col_list: String = cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
        let new_vals: String = cols.iter().map(|c| format!("new.{}", q(c))).collect::<Vec<_>>().join(", ");
        let old_vals: String = cols.iter().map(|c| format!("old.{}", q(c))).collect::<Vec<_>>().join(", ");

        let trig_ai = format!(
            "CREATE TRIGGER {} AFTER INSERT ON {} BEGIN \
                INSERT INTO {}(rowid, {col_list}) VALUES (new.rowid, {new_vals}); \
             END",
            q(&format!("{table}_ai")), q(&table), q(&fts),
        );
        let trig_ad = format!(
            "CREATE TRIGGER {} AFTER DELETE ON {} BEGIN \
                INSERT INTO {}({}, rowid, {col_list}) VALUES ('delete', old.rowid, {old_vals}); \
             END",
            q(&format!("{table}_ad")), q(&table), q(&fts), q(&fts),
        );
        let trig_au = format!(
            "CREATE TRIGGER {} AFTER UPDATE ON {} BEGIN \
                INSERT INTO {}({}, rowid, {col_list}) VALUES ('delete', old.rowid, {old_vals}); \
                INSERT INTO {}(rowid, {col_list}) VALUES (new.rowid, {new_vals}); \
             END",
            q(&format!("{table}_au")), q(&table), q(&fts), q(&fts), q(&fts),
        );

        let populate = format!(
            "INSERT INTO {}(rowid, {col_list}) SELECT rowid, {col_list} FROM {}",
            q(&fts), q(&table),
        );

        let mut script = String::from("BEGIN;\n");
        script.push_str(&create); script.push_str(";\n");
        if create_triggers {
            script.push_str(&trig_ai); script.push_str(";\n");
            script.push_str(&trig_ad); script.push_str(";\n");
            script.push_str(&trig_au); script.push_str(";\n");
        }
        script.push_str(&populate); script.push_str(";\n");
        script.push_str("COMMIT;\n");

        match spi::execute_batch(&script) {
            Ok(_) => text(format!(
                "Enabled FTS5 on {table} ({cols} cols indexed){trig}\n",
                cols = cols.len(),
                trig = if create_triggers { ", triggers installed" } else { "" },
            )),
            Err(e) => {
                let _ = spi::execute_batch("ROLLBACK");
                err(format!(".enable_fts {table}: {}", e.message))
            }
        }
    }

    fn cmd_disable(arg: &str) -> InvokeResult {
        let args = split_args(arg);
        if args.len() != 1 {
            return err(".disable_fts TABLE".into());
        }
        let table = &args[0];
        let fts = format!("{table}_fts");
        let mut script = String::from("BEGIN;\n");
        script.push_str(&format!("DROP TABLE IF EXISTS {};\n", q(&fts)));
        for suffix in ["_ai", "_ad", "_au"] {
            script.push_str(&format!("DROP TRIGGER IF EXISTS {};\n", q(&format!("{table}{suffix}"))));
        }
        script.push_str("COMMIT;\n");
        match spi::execute_batch(&script) {
            Ok(_) => text(format!("Disabled FTS5 on {table}\n")),
            Err(e) => {
                let _ = spi::execute_batch("ROLLBACK");
                err(format!(".disable_fts {table}: {}", e.message))
            }
        }
    }

    fn cmd_rebuild(arg: &str) -> InvokeResult {
        let args = split_args(arg);
        let targets: Vec<String> = if args.is_empty() {
            match list_fts_tables() {
                Ok(v) => v,
                Err(e) => return err(format!(".rebuild_fts: list fts tables: {e}")),
            }
        } else if args.len() == 1 {
            let t = &args[0];
            alloc::vec![format!("{t}_fts")]
        } else {
            return err(".rebuild_fts [TABLE]".into());
        };
        if targets.is_empty() {
            return text("(no fts5 tables found)\n".into());
        }
        let mut rebuilt = 0u32;
        for fts in &targets {
            let sql = format!(
                "INSERT INTO {}({}) VALUES ('rebuild')",
                q(fts), q(fts),
            );
            if let Err(e) = spi::execute(&sql, &[]) {
                return err(format!(".rebuild_fts {fts}: {}", e.message));
            }
            rebuilt += 1;
        }
        text(format!("Rebuilt {rebuilt} fts5 table(s)\n"))
    }

    fn cmd_populate(arg: &str) -> InvokeResult {
        let args = split_args(arg);
        if args.len() < 2 {
            return err(".populate_fts TABLE COL [COL ...]".into());
        }
        let table = &args[0];
        let cols = &args[1..];
        let fts = format!("{table}_fts");
        let col_list: String = cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
        let sql = format!(
            "INSERT INTO {}(rowid, {col_list}) SELECT rowid, {col_list} FROM {}",
            q(&fts), q(table),
        );
        match spi::execute(&sql, &[]) {
            Ok(r) => text(format!("Populated {table}_fts with {} rows\n", r.changes)),
            Err(e) => err(format!(".populate_fts {table}: {}", e.message)),
        }
    }

    fn cmd_search(arg: &str) -> InvokeResult {
        let mut args = split_args(arg);
        let limit_str = split_flag_kv(&mut args, "--limit");
        let columns_str = split_flag_kv(&mut args, "--columns");
        if args.len() < 2 {
            return err(".search TABLE QUERY [--limit N] [--columns col1,...]".into());
        }
        let table = args[0].clone();
        let query = args[1..].join(" "); // allow multi-word match query
        let limit: i64 = limit_str.as_deref().and_then(|s| s.parse().ok()).unwrap_or(20);
        let select_cols = if let Some(s) = columns_str {
            s.split(',').map(|c| q(c.trim())).collect::<Vec<_>>().join(", ")
        } else {
            "*".into()
        };
        let fts = format!("{table}_fts");
        let sql = format!(
            "SELECT {} FROM {} WHERE rowid IN \
               (SELECT rowid FROM {} WHERE {} MATCH ?1 ORDER BY rank LIMIT ?2)",
            select_cols, q(&table), q(&fts), q(&fts),
        );
        let params = alloc::vec![SqlValue::Text(query.clone()), SqlValue::Integer(limit)];
        let result = match spi::execute(&sql, &params) {
            Ok(r) => r,
            Err(e) => return err(format!(".search {table}: {}", e.message)),
        };
        render_rows(&result.columns, &result.rows)
    }

    fn render_rows(columns: &[String], rows: &[Vec<SqlValue>]) -> InvokeResult {
        if rows.is_empty() {
            return text("(no matches)\n".into());
        }
        let mut out = String::new();
        out.push_str(&columns.join("|"));
        out.push('\n');
        for row in rows {
            let parts: Vec<String> = row.iter().map(render_value).collect();
            out.push_str(&parts.join("|"));
            out.push('\n');
        }
        text(out)
    }

    fn render_value(v: &SqlValue) -> String {
        match v {
            SqlValue::Null => String::new(),
            SqlValue::Integer(i) => format!("{i}"),
            SqlValue::Real(r) => format!("{r}"),
            SqlValue::Text(s) => s.clone(),
            SqlValue::Blob(b) => format!("<blob:{} bytes>", b.len()),
        }
    }

    // ---- result helpers ----

    fn spec(id: u64, name: &str, summary: &str, usage: &str, help: &str) -> DotCommandSpec {
        DotCommandSpec {
            id,
            name: name.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            summary: summary.into(),
            usage: usage.into(),
            help: help.into(),
            examples: alloc::vec![],
            requires_write: false,
            no_args: false,
        }
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
