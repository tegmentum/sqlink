//! Cross-DB standard SQL scalars  the portability layer for
//! `greatest`, `least`, `left`, `right`, `lpad`, `rpad`, `repeat`,
//! `space`, `starts_with`, `ends_with`, `translate`, `to_hex`,
//! `bit_length`, `initcap`, plus thin aliases over SQLite builtins
//! (`if`/iif, `chr`/char, `ascii`/unicode, `char_length`/length,
//! `from_hex`/unhex).
//!
//! Surfaces the function set every other DB engine ships
//! (PostgreSQL/MySQL/MariaDB/DuckDB/ClickHouse) so portable SQL
//! doesn't have to be rewritten for SQLite.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

mod algo {
    use alloc::string::String;
    use alloc::vec::Vec;

    pub fn left(s: &str, n: i64) -> String {
        if n <= 0 { return String::new(); }
        s.chars().take(n as usize).collect()
    }

    pub fn right(s: &str, n: i64) -> String {
        if n <= 0 { return String::new(); }
        let chars: Vec<char> = s.chars().collect();
        let start = chars.len().saturating_sub(n as usize);
        chars[start..].iter().collect()
    }

    pub fn lpad(s: &str, len: i64, pad: &str) -> String {
        if len <= 0 || pad.is_empty() {
            return left(s, len.max(0));
        }
        let target = len as usize;
        let cur: Vec<char> = s.chars().collect();
        if cur.len() >= target {
            return cur[..target].iter().collect();
        }
        let pad_chars: Vec<char> = pad.chars().collect();
        let need = target - cur.len();
        let mut out = String::with_capacity(need + s.len());
        for i in 0..need { out.push(pad_chars[i % pad_chars.len()]); }
        out.extend(cur);
        out
    }

    pub fn rpad(s: &str, len: i64, pad: &str) -> String {
        if len <= 0 || pad.is_empty() {
            return left(s, len.max(0));
        }
        let target = len as usize;
        let cur: Vec<char> = s.chars().collect();
        if cur.len() >= target {
            return cur[..target].iter().collect();
        }
        let pad_chars: Vec<char> = pad.chars().collect();
        let need = target - cur.len();
        let mut out: String = cur.iter().collect();
        out.reserve(need);
        for i in 0..need { out.push(pad_chars[i % pad_chars.len()]); }
        out
    }

    pub fn repeat(s: &str, n: i64) -> String {
        if n <= 0 { return String::new(); }
        s.repeat(n as usize)
    }

    pub fn space(n: i64) -> String {
        if n <= 0 { return String::new(); }
        " ".repeat(n as usize)
    }

    pub fn starts_with(s: &str, prefix: &str) -> bool {
        s.starts_with(prefix)
    }

    pub fn ends_with(s: &str, suffix: &str) -> bool {
        s.ends_with(suffix)
    }

    /// Char-by-char map. Chars at position i in `from` map to
    /// position i in `to`; if `to` is shorter (or empty for that
    /// position), the char is dropped. Matches PG / Oracle.
    pub fn translate(s: &str, from: &str, to: &str) -> String {
        let from_chars: Vec<char> = from.chars().collect();
        let to_chars: Vec<char> = to.chars().collect();
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match from_chars.iter().position(|&f| f == c) {
                Some(i) => {
                    if i < to_chars.len() {
                        out.push(to_chars[i]);
                    }
                }
                None => out.push(c),
            }
        }
        out
    }

    pub fn to_hex(n: i64) -> String {
        // PG semantics: lowercase, no leading zeros, two's-complement
        // for negatives. We follow Postgres: negatives use the natural
        // two's-complement repr.
        if n >= 0 {
            alloc::format!("{:x}", n)
        } else {
            alloc::format!("{:x}", n as u64)
        }
    }

    pub fn bit_length(s: &str) -> i64 {
        (s.as_bytes().len() as i64) * 8
    }

    pub fn initcap(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut word_start = true;
        for c in s.chars() {
            if c.is_whitespace() || c == '_' || c == '-' {
                word_start = true;
                out.push(c);
            } else if word_start {
                out.extend(c.to_uppercase());
                word_start = false;
            } else {
                out.extend(c.to_lowercase());
            }
        }
        out
    }

    pub fn char_length(s: &str) -> i64 {
        s.chars().count() as i64
    }

    pub fn ascii(s: &str) -> Option<i64> {
        s.chars().next().map(|c| c as i64)
    }

    pub fn chr(n: i64) -> Option<String> {
        if n < 0 { return None; }
        char::from_u32(n as u32).map(|c| alloc::string::ToString::to_string(&c))
    }

    /// Hex string  bytes. Skips whitespace; rejects odd length or
    /// non-hex chars. Matches SQLite's unhex() behaviour.
    pub fn from_hex(s: &str) -> Result<Vec<u8>, ()> {
        let mut buf = Vec::with_capacity(s.len() / 2);
        let mut nibble: Option<u8> = None;
        for c in s.chars() {
            if c.is_ascii_whitespace() { continue; }
            let h = match c {
                '0'..='9' => c as u8 - b'0',
                'a'..='f' => c as u8 - b'a' + 10,
                'A'..='F' => c as u8 - b'A' + 10,
                _ => return Err(()),
            };
            match nibble {
                None => nibble = Some(h),
                Some(hi) => {
                    buf.push((hi << 4) | h);
                    nibble = None;
                }
            }
        }
        if nibble.is_some() { return Err(()); }
        Ok(buf)
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use crate::algo;
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    // Stable function IDs  each Postgres-flavoured semantic gets
    // exactly one. Multi-arity variants get separate IDs.
    pub const FID_GREATEST:     u64 = 1;
    pub const FID_LEAST:        u64 = 2;
    pub const FID_LEFT:         u64 = 3;
    pub const FID_RIGHT:        u64 = 4;
    pub const FID_LPAD_2:       u64 = 5;
    pub const FID_LPAD_3:       u64 = 6;
    pub const FID_RPAD_2:       u64 = 7;
    pub const FID_RPAD_3:       u64 = 8;
    pub const FID_REPEAT:       u64 = 9;
    pub const FID_SPACE:        u64 = 10;
    pub const FID_STARTS_WITH:  u64 = 11;
    pub const FID_ENDS_WITH:    u64 = 12;
    pub const FID_TRANSLATE:    u64 = 13;
    pub const FID_TO_HEX:       u64 = 14;
    pub const FID_BIT_LENGTH:   u64 = 15;
    pub const FID_INITCAP:      u64 = 16;
    pub const FID_IF:           u64 = 17;
    pub const FID_CHR:          u64 = 18;
    pub const FID_ASCII:        u64 = 19;
    pub const FID_CHAR_LENGTH:  u64 = 20;
    pub const FID_CHARACTER_LENGTH: u64 = 21;
    pub const FID_FROM_HEX:     u64 = 22;
    // ClickHouse camelCase variants  share FIDs with the canonical
    // snake_case names where the semantics line up exactly.
    pub const FID_CH_STARTS_WITH:  u64 = 23;
    pub const FID_CH_ENDS_WITH:    u64 = 24;
    pub const FID_CH_LENGTH:       u64 = 25;
    pub const FID_CH_LOWER_UTF8:   u64 = 26;
    pub const FID_CH_UPPER_UTF8:   u64 = 27;
    pub const FID_CH_TO_STRING:    u64 = 28;
    pub const FID_CH_EMPTY:        u64 = 29;
    pub const FID_CH_NOT_EMPTY:    u64 = 30;
    pub const FID_CH_REPLACE_ALL:  u64 = 31;
    pub const FID_CH_POSITION_UTF8: u64 = 32;
    // PG to_* + quote_* family:
    pub const FID_TO_BIN:        u64 = 33;
    pub const FID_TO_OCT:        u64 = 34;
    pub const FID_TO_ASCII:      u64 = 35;
    pub const FID_QUOTE_IDENT:   u64 = 36;
    pub const FID_QUOTE_LITERAL: u64 = 37;
    pub const FID_QUOTE_NULLABLE: u64 = 38;
    pub const FID_GET_BIT:       u64 = 39;
    pub const FID_SET_BIT:       u64 = 40;
    pub const FID_GET_BYTE:      u64 = 41;
    pub const FID_SET_BYTE:      u64 = 42;

    struct Ext;

    fn as_text(v: &SqlValue, fname: &str, i: usize) -> Result<String, String> {
        match v {
            SqlValue::Text(s) => Ok(s.clone()),
            SqlValue::Integer(n) => Ok(n.to_string()),
            SqlValue::Real(r) => Ok(r.to_string()),
            SqlValue::Blob(b) => Ok(String::from_utf8_lossy(b).into_owned()),
            SqlValue::Null => Err(format!("{fname}: NULL TEXT arg at {i}")),
        }
    }

    fn as_int(v: &SqlValue, fname: &str, i: usize) -> Result<i64, String> {
        match v {
            SqlValue::Integer(n) => Ok(*n),
            SqlValue::Real(r) => Ok(*r as i64),
            SqlValue::Text(s) => s.parse::<i64>().map_err(|_| format!("{fname}: arg {i} not integer")),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    fn cmp_values(a: &SqlValue, b: &SqlValue) -> core::cmp::Ordering {
        use core::cmp::Ordering;
        // SQLite type-affinity comparison: NULL < numeric < text < blob.
        // For greatest/least, NULLs are filtered out before this is
        // called  this remains for the same-bucket comparison.
        match (a, b) {
            (SqlValue::Integer(x), SqlValue::Integer(y)) => x.cmp(y),
            (SqlValue::Real(x), SqlValue::Real(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
            (SqlValue::Integer(x), SqlValue::Real(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
            (SqlValue::Real(x), SqlValue::Integer(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
            (SqlValue::Text(x), SqlValue::Text(y)) => x.cmp(y),
            (SqlValue::Blob(x), SqlValue::Blob(y)) => x.cmp(y),
            // Mixed: numeric < text < blob (matches SQLite).
            (SqlValue::Integer(_) | SqlValue::Real(_), _) => Ordering::Less,
            (_, SqlValue::Integer(_) | SqlValue::Real(_)) => Ordering::Greater,
            (SqlValue::Text(_), SqlValue::Blob(_)) => Ordering::Less,
            (SqlValue::Blob(_), SqlValue::Text(_)) => Ordering::Greater,
            _ => Ordering::Equal,
        }
    }

    fn greatest(args: &[SqlValue]) -> SqlValue {
        args.iter()
            .filter(|v| !matches!(v, SqlValue::Null))
            .max_by(|a, b| cmp_values(a, b))
            .cloned()
            .unwrap_or(SqlValue::Null)
    }

    fn least(args: &[SqlValue]) -> SqlValue {
        args.iter()
            .filter(|v| !matches!(v, SqlValue::Null))
            .min_by(|a, b| cmp_values(a, b))
            .cloned()
            .unwrap_or(SqlValue::Null)
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "stdsql".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_GREATEST,         "greatest",         -1),
                    s(FID_LEAST,            "least",            -1),
                    s(FID_LEFT,             "left",              2),
                    s(FID_RIGHT,            "right",             2),
                    s(FID_LPAD_2,           "lpad",              2),
                    s(FID_LPAD_3,           "lpad",              3),
                    s(FID_RPAD_2,           "rpad",              2),
                    s(FID_RPAD_3,           "rpad",              3),
                    s(FID_REPEAT,           "repeat",            2),
                    s(FID_SPACE,            "space",             1),
                    s(FID_STARTS_WITH,      "starts_with",       2),
                    s(FID_ENDS_WITH,        "ends_with",         2),
                    s(FID_TRANSLATE,        "translate",         3),
                    s(FID_TO_HEX,           "to_hex",            1),
                    s(FID_BIT_LENGTH,       "bit_length",        1),
                    s(FID_INITCAP,          "initcap",           1),
                    s(FID_IF,               "if",                3),
                    s(FID_CHR,              "chr",               1),
                    s(FID_ASCII,            "ascii",             1),
                    s(FID_CHAR_LENGTH,      "char_length",       1),
                    s(FID_CHARACTER_LENGTH, "character_length",  1),
                    s(FID_FROM_HEX,         "from_hex",          1),
                    // ClickHouse camelCase aliases. share FIDs with
                    // the existing implementations where semantics match.
                    s(FID_STARTS_WITH,      "startsWith",        2),
                    s(FID_ENDS_WITH,        "endsWith",          2),
                    s(FID_CH_LENGTH,        "lengthUTF8",        1),
                    s(FID_CH_LOWER_UTF8,    "lowerUTF8",         1),
                    s(FID_CH_UPPER_UTF8,    "upperUTF8",         1),
                    s(FID_CH_TO_STRING,     "toString",          1),
                    s(FID_CH_EMPTY,         "empty",             1),
                    s(FID_CH_NOT_EMPTY,     "notEmpty",          1),
                    s(FID_CH_REPLACE_ALL,   "replaceAll",        3),
                    s(FID_CH_POSITION_UTF8, "positionUTF8",      2),
                    // PostgreSQL to_* + quote_* + bit accessors:
                    s(FID_TO_BIN,        "to_bin",         1),
                    s(FID_TO_OCT,        "to_oct",         1),
                    s(FID_TO_ASCII,      "to_ascii",       1),
                    s(FID_QUOTE_IDENT,   "quote_ident",    1),
                    s(FID_QUOTE_LITERAL, "quote_literal",  1),
                    s(FID_QUOTE_NULLABLE,"quote_nullable", 1),
                    s(FID_GET_BIT,       "get_bit",        2),
                    s(FID_SET_BIT,       "set_bit",        3),
                    s(FID_GET_BYTE,      "get_byte",       2),
                    s(FID_SET_BYTE,      "set_byte",       3),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_GREATEST => Ok(greatest(&args)),
                FID_LEAST => Ok(least(&args)),
                FID_LEFT => {
                    let s = as_text(&args[0], "left", 0)?;
                    let n = as_int(&args[1], "left", 1)?;
                    Ok(SqlValue::Text(algo::left(&s, n)))
                }
                FID_RIGHT => {
                    let s = as_text(&args[0], "right", 0)?;
                    let n = as_int(&args[1], "right", 1)?;
                    Ok(SqlValue::Text(algo::right(&s, n)))
                }
                FID_LPAD_2 => {
                    let s = as_text(&args[0], "lpad", 0)?;
                    let n = as_int(&args[1], "lpad", 1)?;
                    Ok(SqlValue::Text(algo::lpad(&s, n, " ")))
                }
                FID_LPAD_3 => {
                    let s = as_text(&args[0], "lpad", 0)?;
                    let n = as_int(&args[1], "lpad", 1)?;
                    let p = as_text(&args[2], "lpad", 2)?;
                    Ok(SqlValue::Text(algo::lpad(&s, n, &p)))
                }
                FID_RPAD_2 => {
                    let s = as_text(&args[0], "rpad", 0)?;
                    let n = as_int(&args[1], "rpad", 1)?;
                    Ok(SqlValue::Text(algo::rpad(&s, n, " ")))
                }
                FID_RPAD_3 => {
                    let s = as_text(&args[0], "rpad", 0)?;
                    let n = as_int(&args[1], "rpad", 1)?;
                    let p = as_text(&args[2], "rpad", 2)?;
                    Ok(SqlValue::Text(algo::rpad(&s, n, &p)))
                }
                FID_REPEAT => {
                    let s = as_text(&args[0], "repeat", 0)?;
                    let n = as_int(&args[1], "repeat", 1)?;
                    Ok(SqlValue::Text(algo::repeat(&s, n)))
                }
                FID_SPACE => {
                    let n = as_int(&args[0], "space", 0)?;
                    Ok(SqlValue::Text(algo::space(n)))
                }
                FID_STARTS_WITH => {
                    let s = as_text(&args[0], "starts_with", 0)?;
                    let p = as_text(&args[1], "starts_with", 1)?;
                    Ok(SqlValue::Integer(algo::starts_with(&s, &p) as i64))
                }
                FID_ENDS_WITH => {
                    let s = as_text(&args[0], "ends_with", 0)?;
                    let p = as_text(&args[1], "ends_with", 1)?;
                    Ok(SqlValue::Integer(algo::ends_with(&s, &p) as i64))
                }
                FID_TRANSLATE => {
                    let s = as_text(&args[0], "translate", 0)?;
                    let f = as_text(&args[1], "translate", 1)?;
                    let t = as_text(&args[2], "translate", 2)?;
                    Ok(SqlValue::Text(algo::translate(&s, &f, &t)))
                }
                FID_TO_HEX => {
                    let n = as_int(&args[0], "to_hex", 0)?;
                    Ok(SqlValue::Text(algo::to_hex(n)))
                }
                FID_BIT_LENGTH => {
                    let s = as_text(&args[0], "bit_length", 0)?;
                    Ok(SqlValue::Integer(algo::bit_length(&s)))
                }
                FID_INITCAP => {
                    let s = as_text(&args[0], "initcap", 0)?;
                    Ok(SqlValue::Text(algo::initcap(&s)))
                }
                FID_IF => {
                    // if(cond, a, b) => SQLite's iif. Truthiness:
                    // non-zero numeric / non-empty text / non-NULL.
                    let truthy = match &args[0] {
                        SqlValue::Null => false,
                        SqlValue::Integer(n) => *n != 0,
                        SqlValue::Real(r) => *r != 0.0,
                        SqlValue::Text(s) => !s.is_empty(),
                        SqlValue::Blob(b) => !b.is_empty(),
                    };
                    Ok(if truthy { args[1].clone() } else { args[2].clone() })
                }
                FID_CHR => {
                    let n = as_int(&args[0], "chr", 0)?;
                    match algo::chr(n) {
                        Some(s) => Ok(SqlValue::Text(s)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_ASCII => {
                    let s = as_text(&args[0], "ascii", 0)?;
                    match algo::ascii(&s) {
                        Some(n) => Ok(SqlValue::Integer(n)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_CHAR_LENGTH | FID_CHARACTER_LENGTH => {
                    let s = as_text(&args[0], "char_length", 0)?;
                    Ok(SqlValue::Integer(algo::char_length(&s)))
                }
                FID_FROM_HEX => {
                    let s = as_text(&args[0], "from_hex", 0)?;
                    match algo::from_hex(&s) {
                        Ok(b) => Ok(SqlValue::Blob(b)),
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                // ClickHouse camelCase  collapse to the existing
                // canonical semantic. `lengthUTF8` counts chars not
                // bytes; that's exactly `char_length`.
                FID_CH_LENGTH => {
                    let s = as_text(&args[0], "lengthUTF8", 0)?;
                    Ok(SqlValue::Integer(algo::char_length(&s)))
                }
                FID_CH_LOWER_UTF8 => {
                    let s = as_text(&args[0], "lowerUTF8", 0)?;
                    Ok(SqlValue::Text(s.to_lowercase()))
                }
                FID_CH_UPPER_UTF8 => {
                    let s = as_text(&args[0], "upperUTF8", 0)?;
                    Ok(SqlValue::Text(s.to_uppercase()))
                }
                FID_CH_TO_STRING => {
                    // `toString(x)` is SQL-side cast-to-text. Best-
                    // effort coercion mirrors the existing as_text.
                    let s = as_text(&args[0], "toString", 0)?;
                    Ok(SqlValue::Text(s))
                }
                FID_CH_EMPTY => {
                    let s = as_text(&args[0], "empty", 0)?;
                    Ok(SqlValue::Integer(s.is_empty() as i64))
                }
                FID_CH_NOT_EMPTY => {
                    let s = as_text(&args[0], "notEmpty", 0)?;
                    Ok(SqlValue::Integer((!s.is_empty()) as i64))
                }
                FID_CH_REPLACE_ALL => {
                    let s = as_text(&args[0], "replaceAll", 0)?;
                    let from = as_text(&args[1], "replaceAll", 1)?;
                    let to = as_text(&args[2], "replaceAll", 2)?;
                    Ok(SqlValue::Text(s.replace(from.as_str(), &to)))
                }
                FID_TO_BIN => {
                    let n = as_int(&args[0], "to_bin", 0)?;
                    Ok(SqlValue::Text(format!("{:b}", n as u64)))
                }
                FID_TO_OCT => {
                    let n = as_int(&args[0], "to_oct", 0)?;
                    Ok(SqlValue::Text(format!("{:o}", n as u64)))
                }
                FID_TO_ASCII => {
                    // PG to_ascii: best-effort transliteration  here
                    // we drop non-ASCII so the output is pure ASCII.
                    let s = as_text(&args[0], "to_ascii", 0)?;
                    Ok(SqlValue::Text(s.chars().filter(|c| c.is_ascii()).collect()))
                }
                FID_QUOTE_IDENT => {
                    let s = as_text(&args[0], "quote_ident", 0)?;
                    Ok(SqlValue::Text(format!("\"{}\"", s.replace('"', "\"\""))))
                }
                FID_QUOTE_LITERAL => {
                    let s = as_text(&args[0], "quote_literal", 0)?;
                    Ok(SqlValue::Text(format!("'{}'", s.replace('\'', "''"))))
                }
                FID_QUOTE_NULLABLE => {
                    match args.first() {
                        Some(SqlValue::Null) => Ok(SqlValue::Text("NULL".to_string())),
                        _ => {
                            let s = as_text(&args[0], "quote_nullable", 0)?;
                            Ok(SqlValue::Text(format!("'{}'", s.replace('\'', "''"))))
                        }
                    }
                }
                FID_GET_BIT => {
                    let n = as_int(&args[0], "get_bit", 0)?;
                    let i = as_int(&args[1], "get_bit", 1)?;
                    Ok(SqlValue::Integer((((n as u64) >> i) & 1) as i64))
                }
                FID_SET_BIT => {
                    let n = as_int(&args[0], "set_bit", 0)?;
                    let i = as_int(&args[1], "set_bit", 1)?;
                    let v = as_int(&args[2], "set_bit", 2)? & 1;
                    let mask = 1u64 << i;
                    let result = if v == 1 { (n as u64) | mask } else { (n as u64) & !mask };
                    Ok(SqlValue::Integer(result as i64))
                }
                FID_GET_BYTE => {
                    let n = as_int(&args[0], "get_byte", 0)?;
                    let i = as_int(&args[1], "get_byte", 1)?;
                    Ok(SqlValue::Integer((((n as u64) >> (i * 8)) & 0xff) as i64))
                }
                FID_SET_BYTE => {
                    let n = as_int(&args[0], "set_byte", 0)?;
                    let i = as_int(&args[1], "set_byte", 1)?;
                    let v = as_int(&args[2], "set_byte", 2)?;
                    let shift = i * 8;
                    let mask = 0xffu64 << shift;
                    let result = ((n as u64) & !mask) | (((v as u64) & 0xff) << shift);
                    Ok(SqlValue::Integer(result as i64))
                }
                FID_CH_POSITION_UTF8 => {
                    let s = as_text(&args[0], "positionUTF8", 0)?;
                    let n = as_text(&args[1], "positionUTF8", 1)?;
                    let chars: Vec<char> = s.chars().collect();
                    let nchars: Vec<char> = n.chars().collect();
                    let mut idx = 0i64;
                    for i in 0..=chars.len().saturating_sub(nchars.len()) {
                        if chars[i..i + nchars.len()] == *nchars { idx = (i + 1) as i64; break; }
                    }
                    Ok(SqlValue::Integer(idx))
                }
                other => Err(format!("stdsql: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
