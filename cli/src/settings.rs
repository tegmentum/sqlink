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
        }
    }
}

thread_local! {
    pub static SETTINGS: RefCell<Settings> = RefCell::new(Settings::new());
}
