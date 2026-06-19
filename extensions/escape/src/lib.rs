//! Text escaping for URL / HTML / SQL string contexts (hand-rolled, no crate)

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
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

    const FID_URL_ENCODE: u64 = 1;
    const FID_URL_DECODE: u64 = 2;
    const FID_HTML_ESCAPE: u64 = 3;
    const FID_HTML_UNESCAPE: u64 = 4;
    const FID_SQL_QUOTE: u64 = 5;
    const FID_SHELL_QUOTE: u64 = 6;

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
                if s[i..].starts_with("&amp;") { out.push('&'); i += 5; continue; }
                if s[i..].starts_with("&lt;") { out.push('<'); i += 4; continue; }
                if s[i..].starts_with("&gt;") { out.push('>'); i += 4; continue; }
                if s[i..].starts_with("&quot;") { out.push('"'); i += 6; continue; }
                if s[i..].starts_with("&apos;") { out.push('\''); i += 6; continue; }
                if s[i..].starts_with("&#39;") { out.push('\''); i += 5; continue; }
                if let Some(semi) = s[i..].find(';') {
                    let inner = &s[i + 1..i + semi];
                    if let Some(rest) = inner.strip_prefix('#') {
                        let code: Option<u32> = if let Some(hex) = rest.strip_prefix('x').or(rest.strip_prefix('X')) {
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

    struct Ext;

    // ---- Arg helpers ----
    // The Big Three; copy-pasted into every extension. The
    // scaffold ships them so you delete what you don't need.

    #[allow(dead_code)]
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Available flags  pass `det` for deterministic scalars
            // (most cases), `nd` for ones that produce different
            // output each call (rng / time-of-call / counter).
            #[allow(unused_variables)]
            let det = FunctionFlags::DETERMINISTIC;
            #[allow(unused_variables)]
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "escape".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_URL_ENCODE, "url_encode", 1, det),
                    s(FID_URL_DECODE, "url_decode", 1, det),
                    s(FID_HTML_ESCAPE, "html_escape", 1, det),
                    s(FID_HTML_UNESCAPE, "html_unescape", 1, det),
                    s(FID_SQL_QUOTE, "sql_quote", 1, det),
                    s(FID_SHELL_QUOTE, "shell_quote", 1, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "escape")?;

            match func_id {
                FID_URL_ENCODE => Ok(SqlValue::Text(url_encode(&raw))),
                FID_URL_DECODE => Ok(url_decode(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_HTML_ESCAPE => Ok(SqlValue::Text(html_escape(&raw))),
                FID_HTML_UNESCAPE => Ok(SqlValue::Text(html_unescape(&raw))),
                FID_SQL_QUOTE => Ok(SqlValue::Text(sql_quote(&raw))),
                FID_SHELL_QUOTE => Ok(SqlValue::Text(shell_quote(&raw))),
                other => Err(format!("escape: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
