//! extension-functions.c string scalars

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

    const FID_CHARINDEX_2: u64 = 1;
    const FID_CHARINDEX_3: u64 = 2;
    const FID_LEFTSTR: u64 = 3;
    const FID_RIGHTSTR: u64 = 4;
    const FID_REVERSE: u64 = 5;
    const FID_REPLICATE: u64 = 6;
    const FID_PROPER: u64 = 7;
    const FID_PADL: u64 = 8;
    const FID_PADR: u64 = 9;
    const FID_PADC: u64 = 10;
    const FID_STRFILTER: u64 = 11;

    struct Ext;

    /// 1-indexed position of `needle` in `haystack`, optionally
    /// starting search at `start` (also 1-indexed). 0 if not found.
    /// Matches charindex() from extension-functions.c.
    fn charindex(haystack: &str, needle: &str, start: usize) -> i64 {
        if needle.is_empty() {
            return 0;
        }
        let start_idx = if start == 0 { 0 } else { start - 1 };
        // Index by char (not byte) to match SQLite's 1-indexed TEXT semantics.
        let chars: Vec<char> = haystack.chars().collect();
        if start_idx >= chars.len() {
            return 0;
        }
        let nchars: Vec<char> = needle.chars().collect();
        let n = nchars.len();
        for i in start_idx..=chars.len().saturating_sub(n) {
            if chars[i..i + n] == *nchars {
                return (i + 1) as i64;
            }
        }
        0
    }

    fn leftstr(s: &str, n: i64) -> String {
        if n <= 0 {
            return String::new();
        }
        s.chars().take(n as usize).collect()
    }

    fn rightstr(s: &str, n: i64) -> String {
        if n <= 0 {
            return String::new();
        }
        let chars: Vec<char> = s.chars().collect();
        let len = chars.len();
        let start = len.saturating_sub(n as usize);
        chars[start..].iter().collect()
    }

    fn reverse(s: &str) -> String {
        s.chars().rev().collect()
    }

    fn replicate(s: &str, n: i64) -> String {
        if n <= 0 {
            return String::new();
        }
        s.repeat(n as usize)
    }

    /// Title-case: first char of each whitespace-separated word
    /// uppercased, rest lowercased.
    fn proper(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut at_word_start = true;
        for c in s.chars() {
            if c.is_whitespace() {
                at_word_start = true;
                out.push(c);
            } else if at_word_start {
                out.extend(c.to_uppercase());
                at_word_start = false;
            } else {
                out.extend(c.to_lowercase());
            }
        }
        out
    }

    fn padl(s: &str, length: i64) -> String {
        let len = s.chars().count();
        if (len as i64) >= length {
            return s.to_string();
        }
        let pad = (length as usize) - len;
        let mut out = String::with_capacity(s.len() + pad);
        for _ in 0..pad {
            out.push(' ');
        }
        out.push_str(s);
        out
    }

    fn padr(s: &str, length: i64) -> String {
        let len = s.chars().count();
        if (len as i64) >= length {
            return s.to_string();
        }
        let pad = (length as usize) - len;
        let mut out = String::with_capacity(s.len() + pad);
        out.push_str(s);
        for _ in 0..pad {
            out.push(' ');
        }
        out
    }

    fn padc(s: &str, length: i64) -> String {
        let len = s.chars().count();
        if (len as i64) >= length {
            return s.to_string();
        }
        let total_pad = (length as usize) - len;
        let left = total_pad / 2;
        let right = total_pad - left;
        let mut out = String::with_capacity(s.len() + total_pad);
        for _ in 0..left {
            out.push(' ');
        }
        out.push_str(s);
        for _ in 0..right {
            out.push(' ');
        }
        out
    }

    /// Keep only chars from `haystack` that appear in `allowed`.
    fn strfilter(haystack: &str, allowed: &str) -> String {
        let allowed_set: alloc::collections::BTreeSet<char> = allowed.chars().collect();
        haystack
            .chars()
            .filter(|c| allowed_set.contains(c))
            .collect()
    }

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
                name: "extfns".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // Names match extension-functions.c exactly  no
                    // namespace prefix, so callers can drop our
                    // extension in as a replacement.
                    s(FID_CHARINDEX_2, "charindex", 2, det),
                    s(FID_CHARINDEX_3, "charindex", 3, det),
                    s(FID_LEFTSTR, "leftstr", 2, det),
                    s(FID_RIGHTSTR, "rightstr", 2, det),
                    s(FID_REVERSE, "reverse", 1, det),
                    s(FID_REPLICATE, "replicate", 2, det),
                    s(FID_PROPER, "proper", 1, det),
                    s(FID_PADL, "padl", 2, det),
                    s(FID_PADR, "padr", 2, det),
                    s(FID_PADC, "padc", 2, det),
                    s(FID_STRFILTER, "strfilter", 2, det),
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
                FID_CHARINDEX_2 => {
                    let h = arg_text(&args, 0, "charindex")?;
                    let n = arg_text(&args, 1, "charindex")?;
                    Ok(SqlValue::Integer(charindex(&h, &n, 1)))
                }
                FID_CHARINDEX_3 => {
                    let h = arg_text(&args, 0, "charindex")?;
                    let n = arg_text(&args, 1, "charindex")?;
                    let start = arg_int(&args, 2, "charindex")? as usize;
                    Ok(SqlValue::Integer(charindex(&h, &n, start)))
                }
                FID_LEFTSTR => {
                    let s = arg_text(&args, 0, "leftstr")?;
                    let n = arg_int(&args, 1, "leftstr")?;
                    Ok(SqlValue::Text(leftstr(&s, n)))
                }
                FID_RIGHTSTR => {
                    let s = arg_text(&args, 0, "rightstr")?;
                    let n = arg_int(&args, 1, "rightstr")?;
                    Ok(SqlValue::Text(rightstr(&s, n)))
                }
                FID_REVERSE => {
                    let s = arg_text(&args, 0, "reverse")?;
                    Ok(SqlValue::Text(reverse(&s)))
                }
                FID_REPLICATE => {
                    let s = arg_text(&args, 0, "replicate")?;
                    let n = arg_int(&args, 1, "replicate")?;
                    Ok(SqlValue::Text(replicate(&s, n)))
                }
                FID_PROPER => {
                    let s = arg_text(&args, 0, "proper")?;
                    Ok(SqlValue::Text(proper(&s)))
                }
                FID_PADL => {
                    let s = arg_text(&args, 0, "padl")?;
                    let n = arg_int(&args, 1, "padl")?;
                    Ok(SqlValue::Text(padl(&s, n)))
                }
                FID_PADR => {
                    let s = arg_text(&args, 0, "padr")?;
                    let n = arg_int(&args, 1, "padr")?;
                    Ok(SqlValue::Text(padr(&s, n)))
                }
                FID_PADC => {
                    let s = arg_text(&args, 0, "padc")?;
                    let n = arg_int(&args, 1, "padc")?;
                    Ok(SqlValue::Text(padc(&s, n)))
                }
                FID_STRFILTER => {
                    let h = arg_text(&args, 0, "strfilter")?;
                    let a = arg_text(&args, 1, "strfilter")?;
                    Ok(SqlValue::Text(strfilter(&h, &a)))
                }
                other => Err(format!("extfns: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
