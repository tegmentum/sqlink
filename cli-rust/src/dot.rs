//! Dot-command dispatcher.
//!
//! Each `cmd_*` function takes the argument string (everything
//! after the command name) and returns the formatted output the
//! cli's `eval` should emit.

use std::cell::RefCell;

use crate::settings::{self, Mode};
use rusqlite::Connection;

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
        _ => return None,
    })
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
    match conn.prepare(sql) {
        Ok(mut stmt) => {
            let rows = stmt.query_map([pattern], |r| r.get::<_, String>(0));
            match rows {
                Ok(iter) => {
                    let names: Vec<String> = iter.filter_map(|r| r.ok()).collect();
                    if names.is_empty() { String::new() } else { names.join("\n") + "\n" }
                }
                Err(e) => format!("Error: {e}\n"),
            }
        }
        Err(e) => format!("Error: {e}\n"),
    }
}

fn cmd_schema(arg: &str, conn: &Connection) -> String {
    let (sql, params): (&str, Vec<String>) = if arg.is_empty() {
        ("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type, name", vec![])
    } else {
        ("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL AND name = ?1", vec![arg.to_string()])
    };
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => return format!("Error: {e}\n"),
    };
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| r.get::<_, String>(0));
    match rows {
        Ok(iter) => {
            let mut out = String::new();
            for sql in iter.filter_map(|r| r.ok()) {
                out.push_str(&sql);
                if !sql.ends_with(';') { out.push(';'); }
                out.push('\n');
            }
            out
        }
        Err(e) => format!("Error: {e}\n"),
    }
}

fn cmd_indexes(arg: &str, conn: &Connection) -> String {
    let (sql, params): (&str, Vec<String>) = if arg.is_empty() {
        ("SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name", vec![])
    } else {
        ("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 ORDER BY name", vec![arg.to_string()])
    };
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => return format!("Error: {e}\n"),
    };
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| r.get::<_, String>(0));
    match rows {
        Ok(iter) => {
            let names: Vec<String> = iter.filter_map(|r| r.ok()).collect();
            if names.is_empty() { String::new() } else { names.join("\n") + "\n" }
        }
        Err(e) => format!("Error: {e}\n"),
    }
}

fn cmd_databases(conn: &Connection) -> String {
    match conn.prepare("PRAGMA database_list") {
        Ok(mut stmt) => {
            let rows = stmt.query_map([], |r| {
                let seq: i64 = r.get(0)?;
                let name: String = r.get(1)?;
                let file: String = r.get(2)?;
                Ok((seq, name, file))
            });
            match rows {
                Ok(iter) => iter.filter_map(|r| r.ok())
                    .map(|(s, n, f)| format!("{s}: {n} -> {f}"))
                    .collect::<Vec<_>>()
                    .join("\n") + "\n",
                Err(e) => format!("Error: {e}\n"),
            }
        }
        Err(e) => format!("Error: {e}\n"),
    }
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
