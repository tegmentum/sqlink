//! Embed path for escape. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_URL_ENCODE: u64 = 1;
const FID_URL_DECODE: u64 = 2;
const FID_HTML_ESCAPE: u64 = 3;
const FID_HTML_UNESCAPE: u64 = 4;
const FID_SQL_QUOTE: u64 = 5;
const FID_SHELL_QUOTE: u64 = 6;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn is_url_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_url_unreserved(b) {
            out.push(b as char);
        } else {
            let _ = core::fmt::write(&mut out, format_args!("%{:02X}", b));
        }
    }
    out
}

fn url_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn html_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if s[i..].starts_with("&amp;") {
                out.push('&');
                i += 5;
                continue;
            }
            if s[i..].starts_with("&lt;") {
                out.push('<');
                i += 4;
                continue;
            }
            if s[i..].starts_with("&gt;") {
                out.push('>');
                i += 4;
                continue;
            }
            if s[i..].starts_with("&quot;") {
                out.push('"');
                i += 6;
                continue;
            }
            if s[i..].starts_with("&apos;") {
                out.push('\'');
                i += 6;
                continue;
            }
            if s[i..].starts_with("&#39;") {
                out.push('\'');
                i += 5;
                continue;
            }
            if let Some(semi) = s[i..].find(';') {
                let inner = &s[i + 1..i + semi];
                if let Some(rest) = inner.strip_prefix('#') {
                    let code: Option<u32> =
                        if let Some(hex) = rest.strip_prefix('x').or(rest.strip_prefix('X')) {
                            u32::from_str_radix(hex, 16).ok()
                        } else {
                            rest.parse().ok()
                        };
                    if let Some(c) = code.and_then(char::from_u32) {
                        out.push(c);
                        i += semi + 1;
                        continue;
                    }
                }
            }
        }
        let c = s[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn sql_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push('\'');
            out.push('\'');
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

pub fn call_scalar(
    func_id: u64,
    args: alloc::vec::Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "escape")?;
    match func_id {
        FID_URL_ENCODE => Ok(SqlValueOwned::Text(url_encode(&raw))),
        FID_URL_DECODE => Ok(url_decode(&raw)
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_HTML_ESCAPE => Ok(SqlValueOwned::Text(html_escape(&raw))),
        FID_HTML_UNESCAPE => Ok(SqlValueOwned::Text(html_unescape(&raw))),
        FID_SQL_QUOTE => Ok(SqlValueOwned::Text(sql_quote(&raw))),
        FID_SHELL_QUOTE => Ok(SqlValueOwned::Text(shell_quote(&raw))),
        other => Err(format!("escape: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_URL_ENCODE,
        name: b"url_encode\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_URL_DECODE,
        name: b"url_decode\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_HTML_ESCAPE,
        name: b"html_escape\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_HTML_UNESCAPE,
        name: b"html_unescape\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SQL_QUOTE,
        name: b"sql_quote\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SHELL_QUOTE,
        name: b"shell_quote\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
