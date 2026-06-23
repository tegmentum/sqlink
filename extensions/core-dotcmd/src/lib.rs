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
        Guest as DotCommandGuest, InvokeContext, InvokeResult, StateDelta,
    };
    use bindings::exports::sqlite::extension::metadata::{
        DotCommandExample, DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::cli_state;
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
    const FID_PRINT:      u64 = 10;
    const FID_ECHO:       u64 = 11;
    const FID_BAIL:       u64 = 12;
    const FID_TIMER:      u64 = 13;
    const FID_HEADERS:    u64 = 14;
    const FID_MODE:       u64 = 15;
    const FID_NULLVALUE:  u64 = 16;
    const FID_SEPARATOR:  u64 = 17;
    const FID_PROMPT:     u64 = 18;
    const FID_CHANGES:    u64 = 19;
    const FID_STATS:      u64 = 20;
    const FID_EXPLAIN:    u64 = 21;
    const FID_EQP:        u64 = 22;
    const FID_BINARY:     u64 = 23;
    const FID_WIDTH:      u64 = 24;
    const FID_TIMEOUT:    u64 = 25;
    const FID_VFSLIST:    u64 = 26;
    const FID_VFSNAME:    u64 = 27;
    const FID_SHOW:       u64 = 28;
    const FID_LIMIT:      u64 = 29;
    const FID_DBCONFIG:   u64 = 30;
    const FID_PARAMETER:  u64 = 31;

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
                    spec(FID_PRINT, "print",
                         "Echo arguments to stdout",
                         "print [TEXT...]",
                         "Writes the verbatim argument string followed by a newline. \
                          Useful in scripts to annotate output  the cli sees `.print foo bar` \
                          as TEXT='foo bar' and emits 'foo bar\\n'."),
                    spec(FID_ECHO, "echo",
                         "Echo input lines",
                         "echo on|off",
                         "When on, each input line is echoed to stdout before execution."),
                    spec(FID_BAIL, "bail",
                         "Stop after first error",
                         "bail on|off",
                         "When on, a failed SQL statement or dot command aborts the rest of the script."),
                    spec(FID_TIMER, "timer",
                         "Show wall-clock time per statement",
                         "timer on|off",
                         "Appends a `Run Time: real X.XXX` line after each statement when on."),
                    spec(FID_HEADERS, "headers",
                         "Show column headers in result output",
                         "headers on|off",
                         "Toggles whether column names render in list/csv/tabs modes."),
                    spec(FID_MODE, "mode",
                         "Set the result display mode",
                         "mode list|csv|line|column|table|markdown|tabs|json",
                         "Selects the renderer eval_sql uses for SELECT result sets."),
                    spec(FID_NULLVALUE, "nullvalue",
                         "Set the rendering for SQL NULL",
                         "nullvalue STRING",
                         "Replaces empty NULL cells with STRING in list/csv/tabs modes."),
                    spec(FID_SEPARATOR, "separator",
                         "Set the column separator for list/csv/tabs modes",
                         "separator STRING",
                         "Default is `|`. CSV and tabs modes use their canonical separator."),
                    spec(FID_PROMPT, "prompt",
                         "Set the cli prompts",
                         "prompt MAIN [CONT]",
                         "Replaces the default `sqlite> ` and `   ...> ` prompts."),
                    spec(FID_CHANGES, "changes",
                         "Show changes count after each statement",
                         "changes on|off",
                         "Appends `changes: N total_changes: M` after each statement when on."),
                    spec(FID_STATS, "stats",
                         "Show memory stats after each statement",
                         "stats on|off",
                         "Appends `Memory Used: N bytes` after each statement when on."),
                    spec(FID_EXPLAIN, "explain",
                         "Auto-prefix SELECTs with EXPLAIN",
                         "explain on|off|auto",
                         "on: every statement is EXPLAINed. auto: EXPLAIN is applied only when the user typed it."),
                    spec(FID_EQP, "eqp",
                         "Show EXPLAIN QUERY PLAN inline",
                         "eqp on|off",
                         "Prepends the query plan above the statement output when on."),
                    spec(FID_BINARY, "binary",
                         "Print BLOBs as hex literals",
                         "binary on|off",
                         "When on, BLOBs render as `X'…'` hex literals; otherwise `<blob:N bytes>`."),
                    spec(FID_WIDTH, "width",
                         "Set per-column minimum widths",
                         "width [N N N ...]",
                         "Space-separated widths apply as MINIMUMS in column/box/table modes. \
                          Empty list resets to data-driven widths."),
                    spec(FID_TIMEOUT, "timeout",
                         "Set the busy-handler timeout (sqlite3_busy_timeout)",
                         "timeout MS",
                         "Sets how long the cli's connection waits when SQLite reports SQLITE_BUSY. \
                          Applied to the cli's main connection via a state delta."),
                    spec(FID_VFSLIST, "vfslist",
                         "List registered VFSes",
                         "vfslist",
                         "Prints the names of every VFS the host has registered. The first \
                          entry is the default."),
                    spec(FID_VFSNAME, "vfsname",
                         "Print the active VFS for a database",
                         "vfsname [DB]",
                         "DB defaults to `main`. Resolves via the extension's spi connection \
                          (which opens against the same db file, so the VFS matches the cli's)."),
                    spec(FID_SHOW, "show",
                         "Dump current cli settings",
                         "show",
                         "Reads the cli-state snapshot the dispatcher pushes before each \
                          invoke and prints every relevant setting (echo / bail / mode / \
                          prompts / etc.)."),
                    spec(FID_LIMIT, "limit",
                         "Inspect or set sqlite3 per-connection limits",
                         "limit [NAME [VAL]]",
                         "Without args, lists every category and its current value. \
                          With NAME, prints that one. With NAME VAL, sets and prints the \
                          old + new value. Affects the cli's main connection via the \
                          conn/limit/<name> state-delta."),
                    spec(FID_DBCONFIG, "dbconfig",
                         "Inspect or set per-connection db config bools",
                         "dbconfig [OP [VAL]]",
                         "Without args, lists every recognized SQLITE_DBCONFIG_* bool. \
                          With OP, prints just that one. With OP VAL (on/off/0/1), sets \
                          and prints the new value via the conn/db-config/<op> state-delta."),
                    spec(FID_PARAMETER, "parameter",
                         "Manage named SQL parameter bindings",
                         "parameter init|list|set NAME VALUE|unset NAME|clear",
                         "set NAME VALUE binds :NAME / $NAME / @NAME in subsequent \
                          statements. VALUE is parsed as NULL, integer, real, single- \
                          or double-quoted text, or bare text. State-delta keys: \
                          params/set/<name>, params/unset/<name>, params/clear."),
                ],
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

    fn cmd_print(arg: &str) -> InvokeResult {
        cli_stdout::write(arg);
        cli_stdout::write("\n");
        ok()
    }

    /// Helper: emit a single state-delta carrying `value`.
    fn delta(key: &str, value: SqlValue) -> InvokeResult {
        InvokeResult {
            text: String::new(),
            state_deltas: alloc::vec![StateDelta { key: key.into(), value }],
            ok: true,
            exit_code: 0,
        }
    }

    fn parse_onoff(s: &str) -> Option<bool> {
        match s.to_ascii_lowercase().as_str() {
            "on"  | "1" | "true"  | "yes" => Some(true),
            "off" | "0" | "false" | "no"  => Some(false),
            _ => None,
        }
    }

    fn cmd_toggle(key: &str, arg: &str, label: &str) -> InvokeResult {
        match parse_onoff(arg.trim()) {
            Some(b) => delta(key, SqlValue::Integer(if b { 1 } else { 0 })),
            None => err(format!(".{}: expected `on` or `off`, got {:?}", label, arg)),
        }
    }

    fn cmd_set_string(key: &str, arg: &str, label: &str) -> InvokeResult {
        let v = arg.trim();
        if v.is_empty() {
            return err(format!(".{}: missing argument", label));
        }
        delta(key, SqlValue::Text(v.to_string()))
    }

    /// `.prompt MAIN [CONT]`  emit one or two deltas. MAIN is the
    /// first whitespace-delimited token; CONT, if present, is the
    /// rest. Both are quoted-string literals if surrounded by `"…"`.
    fn cmd_prompt(arg: &str) -> InvokeResult {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            return err(".prompt: missing MAIN (and optionally CONT)".to_string());
        }
        // For simplicity v1 splits on the first whitespace run.
        let mut deltas = alloc::vec![];
        if let Some(idx) = trimmed.find(char::is_whitespace) {
            let (main, rest) = trimmed.split_at(idx);
            let cont = rest.trim_start();
            deltas.push(StateDelta {
                key: "prompt/main".into(),
                value: SqlValue::Text(strip_quotes(main).to_string()),
            });
            if !cont.is_empty() {
                deltas.push(StateDelta {
                    key: "prompt/cont".into(),
                    value: SqlValue::Text(strip_quotes(cont).to_string()),
                });
            }
        } else {
            deltas.push(StateDelta {
                key: "prompt/main".into(),
                value: SqlValue::Text(strip_quotes(trimmed).to_string()),
            });
        }
        InvokeResult { text: String::new(), state_deltas: deltas, ok: true, exit_code: 0 }
    }

    /// `.width N N N`  emit `display/width` delta as a single
    /// space-separated string. The cli's applier parses tokens
    /// back into a Vec<usize>; empty arg resets to data-driven.
    fn cmd_width(arg: &str) -> InvokeResult {
        // Validate: every token must parse as a non-negative int.
        // Don't emit a delta if validation fails  fail closed.
        let trimmed = arg.trim();
        for tok in trimmed.split_whitespace() {
            if tok.parse::<isize>().is_err() {
                return err(format!(".width: bad width {tok:?}  expected non-negative integers"));
            }
        }
        delta("display/width", SqlValue::Text(trimmed.into()))
    }

    fn cmd_timeout(arg: &str) -> InvokeResult {
        let s = arg.trim();
        if s.is_empty() {
            return err(".timeout: missing MS".into());
        }
        let ms: i64 = match s.parse() {
            Ok(n) if n >= 0 => n,
            _ => return err(format!(".timeout: bad ms {s:?}")),
        };
        delta("conn/busy-timeout", SqlValue::Integer(ms))
    }

    fn cmd_vfslist() -> InvokeResult {
        let names = spi::list_vfs();
        if names.is_empty() {
            cli_stdout::write("(no VFSes registered)\n");
        } else {
            for (i, name) in names.iter().enumerate() {
                let marker = if i == 0 { " (default)" } else { "" };
                cli_stdout::write(&format!("{name}{marker}\n"));
            }
        }
        ok()
    }

    /// Names mirrored from the cli's `LIMIT_NAMES` / `DBCONFIG_BOOLEANS`
    /// tables. The host's cli-state snapshot exposes the current
    /// values under `conn/limit/<name>` / `conn/db-config/<name>`,
    /// and the set-side state-delta uses the same keys.
    const LIMIT_NAMES_C: &[&str] = &[
        "length", "sql_length", "column", "expr_depth", "compound_select",
        "vdbe_op", "function_arg", "attached", "like_pattern_length",
        "variable_number", "trigger_depth", "worker_threads",
    ];
    const DBCONFIG_NAMES_C: &[&str] = &[
        "defensive", "dqs_dml", "dqs_ddl", "enable_fkey", "enable_trigger",
        "enable_view", "enable_load_extension", "enable_qpsg",
        "legacy_alter_table", "legacy_file_format", "trigger_eqp",
        "trusted_schema", "writable_schema",
    ];

    fn cmd_limit(arg: &str) -> InvokeResult {
        let mut parts = arg.split_whitespace();
        let name = parts.next().unwrap_or("");
        let val = parts.next().unwrap_or("");
        if name.is_empty() {
            // List all.
            let mut out = String::new();
            for n in LIMIT_NAMES_C {
                let v = cli_state::get_int(&format!("conn/limit/{n}"));
                out.push_str(&format!("{:>22} {v}\n", n));
            }
            cli_stdout::write(&out);
            return ok();
        }
        if !LIMIT_NAMES_C.iter().any(|n| *n == name) {
            return err(format!(".limit: unknown category {name:?}"));
        }
        if val.is_empty() {
            let v = cli_state::get_int(&format!("conn/limit/{name}"));
            cli_stdout::write(&format!("{name} {v}\n"));
            return ok();
        }
        let Ok(n) = val.parse::<i64>() else {
            return err(format!(".limit: bad value {val:?}"));
        };
        let prev = cli_state::get_int(&format!("conn/limit/{name}"));
        // Emit a state-delta the cli applies via sqlite3_limit on
        // its main connection. Echo prev + new so users see the
        // change like the upstream cli prints.
        let new_text = format!("{name} {prev} -> {n}\n");
        InvokeResult {
            text: new_text,
            state_deltas: alloc::vec![StateDelta {
                key: format!("conn/limit/{name}"),
                value: SqlValue::Integer(n),
            }],
            ok: true,
            exit_code: 0,
        }
    }

    fn cmd_dbconfig(arg: &str) -> InvokeResult {
        let mut parts = arg.split_whitespace();
        let op = parts.next().unwrap_or("");
        let val = parts.next().unwrap_or("");
        if op.is_empty() {
            let mut out = String::new();
            for n in DBCONFIG_NAMES_C {
                let v = cli_state::get_int(&format!("conn/db-config/{n}"));
                out.push_str(&format!("{:>22} {v}\n", n));
            }
            cli_stdout::write(&out);
            return ok();
        }
        if !DBCONFIG_NAMES_C.iter().any(|n| *n == op) {
            return err(format!(".dbconfig: unknown op {op:?}"));
        }
        if val.is_empty() {
            let v = cli_state::get_int(&format!("conn/db-config/{op}"));
            cli_stdout::write(&format!("{op} {v}\n"));
            return ok();
        }
        let on: bool = match val.to_ascii_lowercase().as_str() {
            "on"  | "1" | "true"  | "yes" => true,
            "off" | "0" | "false" | "no"  => false,
            _ => return err(format!(".dbconfig: bad value {val:?}")),
        };
        let new_text = format!("{op} {}\n", if on { 1 } else { 0 });
        InvokeResult {
            text: new_text,
            state_deltas: alloc::vec![StateDelta {
                key: format!("conn/db-config/{op}"),
                value: SqlValue::Integer(if on { 1 } else { 0 }),
            }],
            ok: true,
            exit_code: 0,
        }
    }

    /// `.parameter`  manages the SQL parameter-binding map the
    /// cli applies before each prepared statement.
    ///
    /// Subcommands:
    ///   (empty / unknown)            usage
    ///   init / clear                 emit params/clear sentinel
    ///   list                         read params/value/* via snapshot
    ///   set NAME VALUE               emit params/set/<bare>(typed)
    ///   unset NAME                   emit params/unset/<bare>
    fn cmd_parameter(arg: &str) -> InvokeResult {
        let mut parts = arg.splitn(3, char::is_whitespace);
        let sub = parts.next().unwrap_or("").trim();
        match sub {
            "" => err_with_usage(),
            "init" | "clear" => {
                // Sentinel: value isn't read by the cli's applier
                // for params/clear; using Integer(1) for consistency.
                InvokeResult {
                    text: String::new(),
                    state_deltas: alloc::vec![StateDelta {
                        key: "params/clear".into(),
                        value: SqlValue::Integer(1),
                    }],
                    ok: true,
                    exit_code: 0,
                }
            }
            "list" => {
                let keys = cli_state::list_keys("params/value/");
                if keys.is_empty() {
                    cli_stdout::write("(no parameters)\n");
                    return ok();
                }
                let mut names: Vec<String> = keys
                    .into_iter()
                    .filter_map(|k| k.strip_prefix("params/value/").map(|s| s.to_string()))
                    .collect();
                names.sort();
                let mut out = String::new();
                for n in &names {
                    // get_text returns the JSON-decoded string the
                    // snapshot stored. For integers/reals the snapshot
                    // encodes bare digits; get_text yields "" then.
                    // Use get_value for a typed read.
                    let v = cli_state::get_value(&format!("params/value/{n}"));
                    out.push_str(&format!("{n} = {}\n", display_sql_value(&v)));
                }
                cli_stdout::write(&out);
                ok()
            }
            "set" => {
                let name = parts.next().unwrap_or("").trim();
                let value = parts.next().unwrap_or("").trim();
                if name.is_empty() || value.is_empty() {
                    return err(".parameter set NAME VALUE  missing args".into());
                }
                let bare = strip_param_sigil(name);
                let typed = parse_param_literal(value);
                InvokeResult {
                    text: String::new(),
                    state_deltas: alloc::vec![StateDelta {
                        key: format!("params/set/{bare}"),
                        value: typed,
                    }],
                    ok: true,
                    exit_code: 0,
                }
            }
            "unset" => {
                let name = parts.next().unwrap_or("").trim();
                if name.is_empty() {
                    return err(".parameter unset NAME  missing arg".into());
                }
                let bare = strip_param_sigil(name);
                InvokeResult {
                    text: String::new(),
                    state_deltas: alloc::vec![StateDelta {
                        key: format!("params/unset/{bare}"),
                        value: SqlValue::Integer(1),
                    }],
                    ok: true,
                    exit_code: 0,
                }
            }
            other => err(format!(".parameter: unknown subcommand {other:?}")),
        }
    }

    fn err_with_usage() -> InvokeResult {
        err(".parameter init|list|set NAME VALUE|unset NAME|clear".into())
    }

    /// Same shape as cli/src/dot.rs::strip_param_sigil  ":foo",
    /// "$foo", "@foo" all return "foo".
    fn strip_param_sigil(name: &str) -> &str {
        match name.as_bytes().first() {
            Some(b':') | Some(b'$') | Some(b'@') => &name[1..],
            _ => name,
        }
    }

    /// Same shape as cli/src/dot.rs::parse_parameter_value
    /// NULL / quoted text / int / float / bare text.
    fn parse_param_literal(raw: &str) -> SqlValue {
        if raw.eq_ignore_ascii_case("null") { return SqlValue::Null; }
        let bytes = raw.as_bytes();
        if bytes.len() >= 2 {
            let first = bytes[0];
            let last = bytes[bytes.len() - 1];
            if (first == b'\'' || first == b'"') && first == last {
                let inner = &raw[1..raw.len() - 1];
                let unesc = if first == b'\'' {
                    inner.replace("''", "'")
                } else {
                    inner.replace("\"\"", "\"")
                };
                return SqlValue::Text(unesc);
            }
        }
        if let Ok(n) = raw.parse::<i64>() { return SqlValue::Integer(n); }
        if let Ok(f) = raw.parse::<f64>() { return SqlValue::Real(f); }
        SqlValue::Text(raw.to_string())
    }

    fn display_sql_value(v: &SqlValue) -> String {
        match v {
            SqlValue::Null       => "NULL".to_string(),
            SqlValue::Integer(i) => i.to_string(),
            SqlValue::Real(r)    => r.to_string(),
            SqlValue::Text(s)    => format!("'{}'", s.replace('\'', "''")),
            SqlValue::Blob(b)    => format!("X'{}'",
                b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
        }
    }

    fn cmd_show() -> InvokeResult {
        let on_off = |b: bool| if b { "on" } else { "off" };
        let echo    = cli_state::get_bool("io/echo");
        let bail    = cli_state::get_bool("bail/on-error");
        let headers = cli_state::get_bool("io/headers");
        let mode    = cli_state::get_text("display/mode");
        let nullv   = cli_state::get_text("display/nullvalue");
        let sep     = cli_state::get_text("display/separator");
        let prompt  = cli_state::get_text("prompt/main");
        let cont    = cli_state::get_text("prompt/cont");
        let timer   = cli_state::get_bool("io/timer");
        let changes = cli_state::get_bool("io/changes");
        let stats   = cli_state::get_bool("io/stats");
        let eqp     = cli_state::get_bool("io/eqp");
        let binary  = cli_state::get_bool("io/binary");
        let explain = cli_state::get_text("io/explain");
        let widths  = cli_state::get_text("display/width");
        let mode_s  = if mode.is_empty() { "list".to_string() } else { mode };
        let explain_s = if explain.is_empty() { "off".to_string() } else { explain };
        let mut o = String::new();
        o.push_str(&format!("        echo: {}\n", on_off(echo)));
        o.push_str(&format!("        bail: {}\n", on_off(bail)));
        o.push_str(&format!("     headers: {}\n", on_off(headers)));
        o.push_str(&format!("        mode: {}\n", mode_s));
        o.push_str(&format!("   nullvalue: {:?}\n", nullv));
        o.push_str(&format!("   separator: {:?}\n", sep));
        o.push_str(&format!("       width: {:?}\n", widths));
        o.push_str(&format!("       timer: {}\n", on_off(timer)));
        o.push_str(&format!("     changes: {}\n", on_off(changes)));
        o.push_str(&format!("       stats: {}\n", on_off(stats)));
        o.push_str(&format!("         eqp: {}\n", on_off(eqp)));
        o.push_str(&format!("      binary: {}\n", on_off(binary)));
        o.push_str(&format!("     explain: {}\n", explain_s));
        o.push_str(&format!("      prompt: {:?}\n", prompt));
        o.push_str(&format!("  contprompt: {:?}\n", cont));
        cli_stdout::write(&o);
        ok()
    }

    fn cmd_vfsname(arg: &str) -> InvokeResult {
        let db = arg.trim();
        let db_name = if db.is_empty() { "main" } else { db };
        match spi::vfs_name(db_name) {
            Ok(name) if name.is_empty() =>
                { cli_stdout::write(&format!("(no vfs name for {db_name})\n")); ok() },
            Ok(name) =>
                { cli_stdout::write(&format!("{name}\n")); ok() },
            Err(e) => err(format!(".vfsname: {}", e.message)),
        }
    }

    fn strip_quotes(s: &str) -> &str {
        if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
            &s[1..s.len() - 1]
        } else {
            s
        }
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
                FID_PRINT     => cmd_print(arg),
                FID_ECHO      => cmd_toggle("io/echo",      arg, "echo"),
                FID_BAIL      => cmd_toggle("bail/on-error",arg, "bail"),
                FID_TIMER     => cmd_toggle("io/timer",     arg, "timer"),
                FID_HEADERS   => cmd_toggle("io/headers",   arg, "headers"),
                FID_CHANGES   => cmd_toggle("io/changes",   arg, "changes"),
                FID_STATS     => cmd_toggle("io/stats",     arg, "stats"),
                FID_EQP       => cmd_toggle("io/eqp",       arg, "eqp"),
                FID_BINARY    => cmd_toggle("io/binary",    arg, "binary"),
                FID_MODE      => cmd_set_string("display/mode",      arg, "mode"),
                FID_NULLVALUE => cmd_set_string("display/nullvalue", arg, "nullvalue"),
                FID_SEPARATOR => cmd_set_string("display/separator", arg, "separator"),
                FID_EXPLAIN   => cmd_set_string("io/explain",        arg, "explain"),
                FID_PROMPT    => cmd_prompt(arg),
                FID_WIDTH     => cmd_width(arg),
                FID_TIMEOUT   => cmd_timeout(arg),
                FID_VFSLIST   => cmd_vfslist(),
                FID_VFSNAME   => cmd_vfsname(arg),
                FID_SHOW      => cmd_show(),
                FID_LIMIT     => cmd_limit(arg),
                FID_DBCONFIG  => cmd_dbconfig(arg),
                FID_PARAMETER => cmd_parameter(arg),
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
