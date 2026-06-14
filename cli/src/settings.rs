//! Per-session CLI settings. Mutable via dot-commands; read by
//! eval to format output.

use std::cell::RefCell;

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
        }
    }
}

thread_local! {
    pub static SETTINGS: RefCell<Settings> = RefCell::new(Settings::new());
}
