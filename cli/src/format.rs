//! Output formatter — takes column names + rows + current
//! Settings and emits the textual representation.

use crate::db::Value;
use crate::settings::{Mode, Settings};

/// Format a result set under the current settings. `rows[i][j]` is
/// the j-th column of the i-th row.
pub fn format(columns: &[String], rows: &[Vec<Value>], s: &Settings) -> String {
    match s.mode {
        Mode::List => fmt_delim(columns, rows, &s.separator, s),
        Mode::Csv => fmt_csv(columns, rows, s),
        Mode::Tabs => fmt_delim(columns, rows, "\t", s),
        Mode::Line => fmt_line(columns, rows, s),
        Mode::Column => fmt_column(columns, rows, s),
        Mode::Table => fmt_table(columns, rows, s),
        Mode::Markdown => fmt_markdown(columns, rows, s),
        Mode::Json => fmt_json(columns, rows, s),
    }
}

fn render(v: &Value, s: &Settings) -> String {
    match v {
        Value::Null => s.null_value.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => r.to_string(),
        Value::Text(t) => t.clone(),
        Value::Blob(b) => {
            if s.binary_output {
                let mut o = String::from("X'");
                for byte in b {
                    o.push_str(&format!("{byte:02x}"));
                }
                o.push('\'');
                o
            } else {
                format!("<blob:{} bytes>", b.len())
            }
        }
    }
}

fn fmt_delim(columns: &[String], rows: &[Vec<Value>], sep: &str, s: &Settings) -> String {
    let mut o = String::new();
    if s.headers {
        o.push_str(&columns.join(sep));
        o.push('\n');
    }
    for row in rows {
        let cells: Vec<String> = row.iter().map(|v| render(v, s)).collect();
        o.push_str(&cells.join(sep));
        o.push('\n');
    }
    o
}

fn fmt_csv(columns: &[String], rows: &[Vec<Value>], s: &Settings) -> String {
    fn esc(field: &str) -> String {
        let needs_quote = field.contains(',') || field.contains('"') || field.contains('\n');
        if needs_quote {
            let escaped = field.replace('"', "\"\"");
            format!("\"{escaped}\"")
        } else {
            field.to_string()
        }
    }
    let mut o = String::new();
    if s.headers {
        let h: Vec<String> = columns.iter().map(|c| esc(c)).collect();
        o.push_str(&h.join(","));
        o.push('\n');
    }
    for row in rows {
        let cells: Vec<String> = row.iter().map(|v| esc(&render(v, s))).collect();
        o.push_str(&cells.join(","));
        o.push('\n');
    }
    o
}

fn fmt_line(columns: &[String], rows: &[Vec<Value>], s: &Settings) -> String {
    let width = columns.iter().map(|c| c.len()).max().unwrap_or(0);
    let mut o = String::new();
    for row in rows {
        for (i, v) in row.iter().enumerate() {
            let name = columns.get(i).map(|s| s.as_str()).unwrap_or("?");
            let val = render(v, s);
            o.push_str(&format!("{name:>width$} = {val}\n", width = width));
        }
        o.push('\n');
    }
    o
}

fn col_widths(columns: &[String], rows: &[Vec<Value>], s: &Settings) -> Vec<usize> {
    let mut w: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    for row in rows {
        for (i, v) in row.iter().enumerate() {
            let cell = render(v, s);
            let cw = cell.chars().count();
            if i < w.len() && cw > w[i] { w[i] = cw; }
        }
    }
    // User-set widths via `.width N N ...` act as a floor.
    for (i, &user_w) in s.column_widths.iter().enumerate() {
        if i < w.len() && user_w > w[i] {
            w[i] = user_w;
        }
    }
    w
}

fn fmt_column(columns: &[String], rows: &[Vec<Value>], s: &Settings) -> String {
    let widths = col_widths(columns, rows, s);
    let mut o = String::new();
    if s.headers {
        for (i, c) in columns.iter().enumerate() {
            o.push_str(&format!("{c:<width$}", width = widths[i]));
            if i + 1 < columns.len() { o.push_str("  "); }
        }
        o.push('\n');
        for (i, _) in columns.iter().enumerate() {
            o.push_str(&"-".repeat(widths[i]));
            if i + 1 < columns.len() { o.push_str("  "); }
        }
        o.push('\n');
    }
    for row in rows {
        for (i, v) in row.iter().enumerate() {
            let cell = render(v, s);
            let w = *widths.get(i).unwrap_or(&cell.len());
            o.push_str(&format!("{cell:<w$}"));
            if i + 1 < row.len() { o.push_str("  "); }
        }
        o.push('\n');
    }
    o
}

fn fmt_table(columns: &[String], rows: &[Vec<Value>], s: &Settings) -> String {
    let widths = col_widths(columns, rows, s);
    let sep = {
        let mut s = String::from("+");
        for w in &widths {
            s.push_str(&"-".repeat(w + 2));
            s.push('+');
        }
        s.push('\n');
        s
    };
    let mut o = String::new();
    o.push_str(&sep);
    if s.headers {
        o.push('|');
        for (i, c) in columns.iter().enumerate() {
            o.push_str(&format!(" {c:<w$} |", w = widths[i]));
            let _ = i;
        }
        o.push('\n');
        o.push_str(&sep);
    }
    for row in rows {
        o.push('|');
        for (i, v) in row.iter().enumerate() {
            let cell = render(v, s);
            let w = *widths.get(i).unwrap_or(&cell.len());
            o.push_str(&format!(" {cell:<w$} |"));
        }
        o.push('\n');
    }
    o.push_str(&sep);
    o
}

fn fmt_markdown(columns: &[String], rows: &[Vec<Value>], s: &Settings) -> String {
    let widths = col_widths(columns, rows, s);
    let mut o = String::new();
    // Header always present in markdown
    o.push('|');
    for (i, c) in columns.iter().enumerate() {
        o.push_str(&format!(" {c:<w$} |", w = widths[i]));
    }
    o.push('\n');
    o.push('|');
    for w in &widths {
        o.push(' ');
        o.push_str(&"-".repeat(*w));
        o.push_str(" |");
    }
    o.push('\n');
    for row in rows {
        o.push('|');
        for (i, v) in row.iter().enumerate() {
            let cell = render(v, s).replace('|', "\\|");
            let w = *widths.get(i).unwrap_or(&cell.len());
            o.push_str(&format!(" {cell:<w$} |"));
        }
        o.push('\n');
    }
    o
}

fn fmt_json(columns: &[String], rows: &[Vec<Value>], _s: &Settings) -> String {
    fn esc_str(t: &str) -> String {
        let mut o = String::from("\"");
        for c in t.chars() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                '\n' => o.push_str("\\n"),
                '\r' => o.push_str("\\r"),
                '\t' => o.push_str("\\t"),
                c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                c => o.push(c),
            }
        }
        o.push('"');
        o
    }
    fn esc_val(v: &Value) -> String {
        match v {
            Value::Null => "null".to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Real(r) => r.to_string(),
            Value::Text(t) => esc_str(t),
            Value::Blob(b) => esc_str(&format!("<blob:{} bytes>", b.len())),
        }
    }
    let mut o = String::from("[");
    for (ri, row) in rows.iter().enumerate() {
        if ri > 0 { o.push(','); }
        o.push('{');
        for (ci, v) in row.iter().enumerate() {
            if ci > 0 { o.push(','); }
            o.push_str(&esc_str(columns.get(ci).map(|s| s.as_str()).unwrap_or("?")));
            o.push(':');
            o.push_str(&esc_val(v));
        }
        o.push('}');
    }
    o.push_str("]\n");
    o
}
