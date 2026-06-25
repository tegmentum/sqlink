//! Natural sort comparison (file2 < file10)

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

// wasm_export is gated off in embed builds  the WIT export
// symbols would collide with any other embedded extension's.
// See PLAN-embed-extensions.md.
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

    const FID_COMPARE: u64 = 1;
    const FID_KEY: u64 = 2;
    const FID_LESS: u64 = 3;

    struct Ext;

    /// Token is either an integer (with leading zeros preserved for
    /// pad info but compared numerically) or a text segment compared
    /// case-insensitively.
    enum Tok {
        Num(u64, usize), // (value, original digit count for tie-break)
        Text(String),
    }

    /// Tokenize "abc123def" into [Text("abc"), Num(123, 3), Text("def")].
    /// Leading zeros captured in the digit count so "01" < "1" as a
    /// tie-break when numeric values are equal.
    fn tokenize(s: &str) -> Vec<Tok> {
        let mut out: Vec<Tok> = alloc::vec![];
        let mut buf = String::new();
        let mut in_digits = false;
        for c in s.chars() {
            let is_d = c.is_ascii_digit();
            if is_d != in_digits && !buf.is_empty() {
                flush(&mut out, &mut buf, in_digits);
                buf = String::new();
            }
            in_digits = is_d;
            buf.push(c);
        }
        if !buf.is_empty() {
            flush(&mut out, &mut buf, in_digits);
        }
        out
    }

    fn flush(out: &mut Vec<Tok>, buf: &mut String, was_digits: bool) {
        if was_digits {
            let len = buf.len();
            let val = buf.parse::<u64>().unwrap_or(u64::MAX);
            out.push(Tok::Num(val, len));
        } else {
            out.push(Tok::Text(buf.to_lowercase()));
        }
        buf.clear();
    }

    /// Natural compare. Returns -1, 0, or 1.
    fn compare(a: &str, b: &str) -> i64 {
        let ta = tokenize(a);
        let tb = tokenize(b);
        for (xa, xb) in ta.iter().zip(tb.iter()) {
            let c = match (xa, xb) {
                (Tok::Num(va, la), Tok::Num(vb, lb)) => match va.cmp(vb) {
                    core::cmp::Ordering::Equal => la.cmp(lb),
                    o => o,
                },
                (Tok::Text(sa), Tok::Text(sb)) => sa.cmp(sb),
                // Mixed tokens at the same position: numbers sort
                // before text. ("file10" before "filea"
                // standard natsort convention.)
                (Tok::Num(_, _), Tok::Text(_)) => core::cmp::Ordering::Less,
                (Tok::Text(_), Tok::Num(_, _)) => core::cmp::Ordering::Greater,
            };
            match c {
                core::cmp::Ordering::Less => return -1,
                core::cmp::Ordering::Greater => return 1,
                core::cmp::Ordering::Equal => continue,
            }
        }
        // All shared positions tied. Shorter sorts first.
        match ta.len().cmp(&tb.len()) {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        }
    }

    /// Stable sort key. Produces a packed string that lexicographic
    /// comparison agrees with natural comparison. Useful when you
    /// want to ORDER BY but the storage engine doesn't speak `compare`.
    /// Numbers padded to 20 digits with leading zeros so up to
    /// u64::MAX sorts correctly under bytewise comparison.
    fn key(s: &str) -> String {
        let toks = tokenize(s);
        let mut out = String::with_capacity(s.len() + toks.len() * 20);
        for t in &toks {
            match t {
                Tok::Num(v, _) => {
                    // 'N' tag sorts before 'T' (numbers first at any
                    // position, matching the comparator).
                    out.push('N');
                    let zero_padded = format!("{:020}", v);
                    out.push_str(&zero_padded);
                }
                Tok::Text(s) => {
                    out.push('T');
                    out.push_str(s);
                    out.push('\0'); // terminator so "ab"+next != "abc"
                }
            }
        }
        out
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
                name: "natsort".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_COMPARE, "natsort_compare", 2, det),
                    s(FID_KEY, "natsort_key", 1, det),
                    s(FID_LESS, "natsort_less", 2, det),
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
                preferred_prefix: Some("natsort".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.natsort".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_COMPARE => {
                    let a = arg_text(&args, 0, "natsort_compare")?;
                    let b = arg_text(&args, 1, "natsort_compare")?;
                    Ok(SqlValue::Integer(compare(&a, &b)))
                }
                FID_KEY => {
                    let s = arg_text(&args, 0, "natsort_key")?;
                    Ok(SqlValue::Text(key(&s)))
                }
                FID_LESS => {
                    let a = arg_text(&args, 0, "natsort_less")?;
                    let b = arg_text(&args, 1, "natsort_less")?;
                    Ok(SqlValue::Integer(if compare(&a, &b) < 0 { 1 } else { 0 }))
                }
                other => Err(format!("natsort: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
