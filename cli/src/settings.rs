//! Per-session CLI settings. Mutable via dot-commands; read by
//! eval to format output.

use std::cell::RefCell;
use std::collections::HashMap;

use sqlite_wasm_core::db;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExplainMode {
    Off,
    On,
    Auto,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    List,
    Csv,
    Line,
    Column,
    Table,
    Markdown,
    Tabs,
    Json,
}

impl Mode {
    pub fn name(&self) -> &'static str {
        match self {
            Mode::List => "list",
            Mode::Csv => "csv",
            Mode::Line => "line",
            Mode::Column => "column",
            Mode::Table => "table",
            Mode::Markdown => "markdown",
            Mode::Tabs => "tabs",
            Mode::Json => "json",
        }
    }
}

#[derive(Clone)]
pub struct Settings {
    pub mode: Mode,
    pub headers: bool,
    pub echo: bool,
    pub bail: bool,
    pub null_value: String,
    pub separator: String,
    pub prompt_main: String,
    pub prompt_cont: String,
    /// User-set per-column widths (sqlite3's `.width N N ...`).
    /// Acts as a MINIMUM in column/box/table modes; the actual
    /// width is `max(user-width, data-width)`. Empty = no user
    /// override; everything is data-driven.
    pub column_widths: Vec<usize>,
    /// `.changes on|off` — append "changes: N total_changes: M"
    /// after each statement when on.
    pub show_changes: bool,
    /// `.timer on|off` — append "Run Time: real X.XXX" after each
    /// statement when on.
    pub show_timer: bool,
    /// `.output FILE` — when Some(path), eval output goes to this
    /// file (append after the .output command itself truncates it).
    /// None means stdout. Cleared by `.output` / `.output stdout`.
    pub output_path: Option<String>,
    /// `.once FILE` — when Some(path), the NEXT statement's output
    /// goes to this file (truncate-write), then this field is
    /// cleared. Takes precedence over output_path for one statement.
    pub once_output_path: Option<String>,
    /// `.explain on|off|auto` — when On, eval_sql prefixes the
    /// user's SQL with `EXPLAIN`. Auto turns it on for queries
    /// whose first keyword is `EXPLAIN`.
    pub explain_mode: ExplainMode,
    /// `.eqp on|off` — when on, eval_sql prepends
    /// `EXPLAIN QUERY PLAN <sql>` output before the statement.
    pub eqp: bool,
    /// `.stats on|off` — when on, append a `Memory Used: N bytes`
    /// line after each statement.
    pub show_stats: bool,
    /// `.trace on|off` — when on, the connection's
    /// sqlite3_trace_v2 callback appends expanded SQL to
    /// TRACE_BUF before each statement runs.
    pub trace_on: bool,
    /// `.parameter set NAME VALUE` — named parameter bindings
    /// the cli applies to prepared statements whose
    /// `bind_parameter_name(i)` matches `:NAME` / `$NAME` /
    /// `@NAME`. Cleared by `.parameter init` / `.parameter clear`.
    pub parameters: HashMap<String, db::Value>,
    /// `.binary on|off` — when on, BLOBs print as `X'…'` hex
    /// literals (the SQL-quotable form). When off (default),
    /// `<blob:N bytes>` placeholder. We don't dump raw bytes to
    /// the output channel — that breaks the String-based format
    /// pipeline.
    pub binary_output: bool,
    /// `.log on|off|FILE` state. None = disabled. Some(None) =
    /// enabled, stderr destination. Some(Some(path)) = enabled,
    /// writes append to `path`. The core's process-global
    /// sqlite3 log callback (installed in run() before
    /// init_wasivfs) reads this on every log event.
    pub log_target: Option<Option<String>>,
}

impl Settings {
    pub fn new() -> Self {
        Self {
            mode: Mode::List,
            headers: false,
            echo: false,
            bail: false,
            null_value: String::new(),
            separator: "|".to_string(),
            prompt_main: "sqlite> ".to_string(),
            prompt_cont: "   ...> ".to_string(),
            column_widths: Vec::new(),
            show_changes: false,
            show_timer: false,
            output_path: None,
            once_output_path: None,
            explain_mode: ExplainMode::Off,
            eqp: false,
            show_stats: false,
            trace_on: false,
            parameters: HashMap::new(),
            binary_output: false,
            log_target: None,
        }
    }
}

// Buffer the trace callback appends to. Drained by `eval_sql`
// after each statement so the captured lines render inline with
// the result.
thread_local! {
    pub static TRACE_BUF: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

thread_local! {
    pub static SETTINGS: RefCell<Settings> = RefCell::new(Settings::new());
}

/// Apply a single state-delta returned by a dot-command extension.
/// `value_json` is JSON-encoded (so the wit boundary doesn't have to
/// track sql-value variants). Keys are slash-namespaced per
/// PLAN-dotcmd-plugins.md state schema.
///
/// Unknown keys are silently ignored  v1 covers the common
/// session-mode commands. Decode failures log to stderr and
/// otherwise no-op (a misbehaving extension shouldn't kill the
/// cli).
pub fn apply_dotcmd_delta(key: &str, value_json: &str) {
    SETTINGS.with(|s| {
        let mut g = s.borrow_mut();
        match key {
            "io/echo"        => if let Some(b) = parse_bool(value_json)   { g.echo = b; },
            "io/headers"     => if let Some(b) = parse_bool(value_json)   { g.headers = b; },
            "io/timer"       => if let Some(b) = parse_bool(value_json)   { g.show_timer = b; },
            "io/stats"       => if let Some(b) = parse_bool(value_json)   { g.show_stats = b; },
            "io/changes"     => if let Some(b) = parse_bool(value_json)   { g.show_changes = b; },
            "io/binary"      => if let Some(b) = parse_bool(value_json)   { g.binary_output = b; },
            "io/eqp"         => if let Some(b) = parse_bool(value_json)   { g.eqp = b; },
            "io/explain"     => if let Some(s) = parse_string(value_json) {
                g.explain_mode = match s.as_str() {
                    "on"   => ExplainMode::On,
                    "auto" => ExplainMode::Auto,
                    _      => ExplainMode::Off,
                };
            },
            "io/trace"       => if let Some(b) = parse_bool(value_json)   { g.trace_on = b; },
            "bail/on-error"  => if let Some(b) = parse_bool(value_json)   { g.bail = b; },
            "display/mode"   => if let Some(s) = parse_string(value_json) {
                g.mode = match s.as_str() {
                    "csv"      => Mode::Csv,
                    "line"     => Mode::Line,
                    "column"   => Mode::Column,
                    "table"    => Mode::Table,
                    "markdown" => Mode::Markdown,
                    "tabs"     => Mode::Tabs,
                    "json"     => Mode::Json,
                    _          => Mode::List,
                };
            },
            "display/nullvalue" => if let Some(s) = parse_string(value_json) { g.null_value = s; },
            "display/separator" => if let Some(s) = parse_string(value_json) { g.separator = s; },
            "display/width"     => if let Some(s) = parse_string(value_json) {
                // Space-separated non-negative ints; empty resets.
                let mut widths = Vec::new();
                let mut bad = false;
                for tok in s.split_whitespace() {
                    match tok.parse::<isize>() {
                        Ok(n) => widths.push(n.max(0) as usize),
                        Err(_) => { bad = true; break; }
                    }
                }
                if !bad { g.column_widths = widths; }
            },
            "prompt/main"       => if let Some(s) = parse_string(value_json) { g.prompt_main = s; },
            "prompt/cont"       => if let Some(s) = parse_string(value_json) { g.prompt_cont = s; },
            "conn/busy-timeout" => if let Some(ms) = parse_int(value_json) {
                // Apply to the cli's main connection. Extensions
                // run their own spi connection; setting busy_timeout
                // there wouldn't help the cli's user-facing
                // statements, so the delta path is the only way to
                // affect what the cli sees.
                if let Some(conn_ms) = i32::try_from(ms).ok() {
                    crate::CLI_CONN.with(|c| {
                        let g = c.borrow();
                        if let Some(conn) = g.as_ref() {
                            let _ = conn.busy_timeout(conn_ms);
                        }
                    });
                }
            },
            "params/clear" => {
                g.parameters.clear();
            }
            other => {
                // Map-shaped deltas with a `/<name>` suffix.
                if let Some(name) = other.strip_prefix("params/set/") {
                    g.parameters.insert(name.to_string(), parse_param_value(value_json));
                } else if let Some(name) = other.strip_prefix("params/unset/") {
                    g.parameters.remove(name);
                } else
                // Connection-level deltas with a `/<name>` suffix.
                if let Some(name) = other.strip_prefix("conn/limit/") {
                    if let (Some(code), Some(n)) =
                        (crate::limit_code(name), parse_int(value_json))
                    {
                        if let Ok(v) = i32::try_from(n) {
                            crate::CLI_CONN.with(|c| {
                                let cg = c.borrow();
                                if let Some(conn) = cg.as_ref() {
                                    let _ = conn.limit(code, v);
                                }
                            });
                        }
                    }
                } else if let Some(name) = other.strip_prefix("conn/db-config/") {
                    if let (Some(code), Some(b)) =
                        (crate::dbconfig_code(name), parse_bool(value_json))
                    {
                        crate::CLI_CONN.with(|c| {
                            let cg = c.borrow();
                            if let Some(conn) = cg.as_ref() {
                                let _ = conn.db_config_set_bool(code, b);
                            }
                        });
                    }
                }
                // Other unknown keys are silently ignored.
            }
        }
    });
}

fn parse_int(json: &str) -> Option<i64> {
    json.trim().parse::<i64>().ok()
}

/// Decode a state-delta value-json into a db::Value. Used by the
/// `params/set/<name>` delta handler  the extension sends an
/// SqlValue, the host JSON-encodes it (Integer  bare, Real
/// bare, Text  "...", Blob  null sentinel, Null  null).
fn parse_param_value(json: &str) -> db::Value {
    let t = json.trim();
    if t == "null" { return db::Value::Null; }
    if t == "true" { return db::Value::Integer(1); }
    if t == "false" { return db::Value::Integer(0); }
    if let Ok(i) = t.parse::<i64>() { return db::Value::Integer(i); }
    if let Ok(f) = t.parse::<f64>() { return db::Value::Real(f); }
    if let Some(s) = parse_string(json) { return db::Value::Text(s); }
    db::Value::Null
}

/// Decode a JSON boolean. Accepts the JSON forms `true`/`false`, and
/// also the integers `0`/`1` (the wasm side often passes booleans
/// through as Integer 0/1).
fn parse_bool(json: &str) -> Option<bool> {
    match json.trim() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// Decode a JSON string literal. v1 keeps it minimal  unescapes
/// `\\"`, `\\\\`, `\\n`, `\\r`, `\\t`. Anything else is passed
/// through verbatim.
fn parse_string(json: &str) -> Option<String> {
    let s = json.trim();
    if !s.starts_with('"') || !s.ends_with('"') || s.len() < 2 {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => { out.push('\\'); out.push(other); }
            None => out.push('\\'),
        }
    }
    Some(out)
}
