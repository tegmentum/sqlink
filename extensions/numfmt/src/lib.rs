//! Number formatting: commas, ordinals, scientific, percent

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

    const FID_COMMAS: u64 = 1;
    const FID_FIXED: u64 = 2;
    const FID_ORDINAL: u64 = 3;
    const FID_SCIENTIFIC: u64 = 4;
    const FID_PERCENT: u64 = 5;
    const FID_PAD: u64 = 6;
    const FID_GROUP: u64 = 7;

    struct Ext;

    /// Insert thousands separators every 3 digits from the right of
    /// the integer part. Preserves leading sign and any decimal tail.
    fn with_separators(s: &str, sep: char) -> String {
        let (sign, rest) = if let Some(r) = s.strip_prefix('-') {
            ("-", r)
        } else {
            ("", s)
        };
        let (intp, decp) = match rest.find('.') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        // Walk digits from right to left, insert sep every 3.
        let chars: Vec<char> = intp.chars().rev().collect();
        let mut out: Vec<char> = alloc::vec![];
        for (i, c) in chars.iter().enumerate() {
            if i > 0 && i % 3 == 0 {
                out.push(sep);
            }
            out.push(*c);
        }
        let intp_grouped: String = out.into_iter().rev().collect();
        format!("{sign}{intp_grouped}{decp}")
    }

    fn commas(n: f64, places: i64) -> String {
        let formatted = if places >= 0 {
            format!("{:.*}", places as usize, n)
        } else {
            format!("{n}")
        };
        with_separators(&formatted, ',')
    }

    fn fixed(n: f64, places: i64) -> String {
        let p = if places < 0 { 0 } else { places as usize };
        format!("{:.*}", p, n)
    }

    /// English ordinal suffix for an integer: 1st, 2nd, 3rd, 4th,
    /// 11th, 21st, 22nd, 101st, ...
    fn ordinal(n: i64) -> String {
        let abs = n.unsigned_abs();
        // 11/12/13 are -th regardless of last digit.
        let suffix = match abs % 100 {
            11 | 12 | 13 => "th",
            _ => match abs % 10 {
                1 => "st",
                2 => "nd",
                3 => "rd",
                _ => "th",
            },
        };
        format!("{n}{suffix}")
    }

    fn scientific(n: f64, sig: i64) -> String {
        let s = if sig < 0 { 6 } else { sig as usize };
        if s == 0 {
            return format!("{:e}", n);
        }
        format!("{:.*e}", s.saturating_sub(1), n)
    }

    fn percent(n: f64, places: i64) -> String {
        let p = if places < 0 { 1 } else { places as usize };
        format!("{:.*}%", p, n * 100.0)
    }

    fn pad_left(s: &str, width: i64, fill: &str) -> String {
        let w = if width < 0 { 0 } else { width as usize };
        let fillc = fill.chars().next().unwrap_or(' ');
        if s.chars().count() >= w {
            return s.to_string();
        }
        let pad_n = w - s.chars().count();
        let mut out = String::with_capacity(s.len() + pad_n);
        for _ in 0..pad_n { out.push(fillc); }
        out.push_str(s);
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
                name: "numfmt".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_COMMAS, "numfmt_commas", 2, det),
                    s(FID_FIXED, "numfmt_fixed", 2, det),
                    s(FID_ORDINAL, "numfmt_ordinal", 1, det),
                    s(FID_SCIENTIFIC, "numfmt_scientific", 2, det),
                    s(FID_PERCENT, "numfmt_percent", 2, det),
                    s(FID_PAD, "numfmt_pad_left", 3, det),
                    s(FID_GROUP, "numfmt_group", 2, det),
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

    fn arg_real(args: &[SqlValue], i: usize, fname: &str) -> Result<f64, String> {
        match args.get(i) {
            Some(SqlValue::Real(r)) => Ok(*r),
            Some(SqlValue::Integer(n)) => Ok(*n as f64),
            _ => Err(format!("{fname}: numeric arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_COMMAS => {
                    let n = arg_real(&args, 0, "numfmt_commas")?;
                    let p = arg_int(&args, 1, "numfmt_commas")?;
                    Ok(SqlValue::Text(commas(n, p)))
                }
                FID_FIXED => {
                    let n = arg_real(&args, 0, "numfmt_fixed")?;
                    let p = arg_int(&args, 1, "numfmt_fixed")?;
                    Ok(SqlValue::Text(fixed(n, p)))
                }
                FID_ORDINAL => {
                    let n = arg_int(&args, 0, "numfmt_ordinal")?;
                    Ok(SqlValue::Text(ordinal(n)))
                }
                FID_SCIENTIFIC => {
                    let n = arg_real(&args, 0, "numfmt_scientific")?;
                    let s = arg_int(&args, 1, "numfmt_scientific")?;
                    Ok(SqlValue::Text(scientific(n, s)))
                }
                FID_PERCENT => {
                    let n = arg_real(&args, 0, "numfmt_percent")?;
                    let p = arg_int(&args, 1, "numfmt_percent")?;
                    Ok(SqlValue::Text(percent(n, p)))
                }
                FID_PAD => {
                    let s = arg_text(&args, 0, "numfmt_pad_left")?;
                    let w = arg_int(&args, 1, "numfmt_pad_left")?;
                    let f = arg_text(&args, 2, "numfmt_pad_left")?;
                    Ok(SqlValue::Text(pad_left(&s, w, &f)))
                }
                FID_GROUP => {
                    // numfmt_group(n, sep_char_or_string): commas with
                    // custom separator. Useful for European "1.234,56"
                    // style (call with '.').
                    let n = arg_real(&args, 0, "numfmt_group")?;
                    let sep_s = arg_text(&args, 1, "numfmt_group")?;
                    let sep = sep_s.chars().next().unwrap_or(',');
                    Ok(SqlValue::Text(with_separators(&format!("{n}"), sep)))
                }
                other => Err(format!("numfmt: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
