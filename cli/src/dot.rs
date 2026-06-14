//! Dot-command dispatcher.
//!
//! Each `cmd_*` function takes the argument string (everything
//! after the command name) and returns the formatted output the
//! cli's `eval` should emit.

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
        ".help" => cmd_help(),
        ".show" => cmd_show(),
        ".tables" => cmd_tables(arg, conn),
        ".schema" => cmd_schema(arg, conn),
        ".indexes" => cmd_indexes(arg, conn),
        ".databases" => cmd_databases(conn),
        ".headers" => cmd_headers(arg),
        ".mode" => cmd_mode(arg),
        ".nullvalue" => cmd_nullvalue(arg),
        ".separator" => cmd_separator(arg),
        ".echo" => cmd_echo(arg),
        ".prompt" => cmd_prompt(arg),
        ".print" => format!("{arg}\n"),
        ".bail" => cmd_bail(arg),
        ".version" => cmd_version(),
        ".width" => cmd_width(arg),
        ".changes" => cmd_changes(arg),
        ".timer" => cmd_timer(arg),
        ".timeout" => cmd_timeout(arg, conn),
        ".explain" => cmd_explain(arg),
        ".eqp" => cmd_eqp(arg),
        ".stats" => cmd_stats(arg),
        ".parameter" => cmd_parameter(arg),
        ".fullschema" => cmd_fullschema(conn),
        ".dbinfo" => cmd_dbinfo(arg, conn),
        ".dbconfig" => cmd_dbconfig(arg, conn),
        ".limit" => cmd_limit(arg, conn),
        ".binary" => cmd_binary(arg),
        ".log" => "Error: .log is not supported (sqlite3 config-time only)\n".to_string(),
        _ => return None,
    })
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
