//! Humanize bytes + durations; bidirectional parse

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

    const FID_BYTES: u64 = 1;
    const FID_IBYTES: u64 = 2;
    const FID_PARSE_BYTES: u64 = 3;
    const FID_DURATION: u64 = 4;
    const FID_PARSE_DURATION: u64 = 5;

    struct Ext;

    /// Format n bytes with decimal (KB=1000) prefixes.
    /// Picks the largest unit where value 1.0.
    fn format_bytes(n: f64, binary: bool) -> String {
        let (base, units): (f64, &[&str]) = if binary {
            (1024.0, &["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"])
        } else {
            (1000.0, &["B", "KB", "MB", "GB", "TB", "PB", "EB"])
        };
        let mut v = n.abs();
        let mut i = 0;
        while v >= base && i < units.len() - 1 {
            v /= base;
            i += 1;
        }
        let sign = if n < 0.0 { "-" } else { "" };
        if i == 0 {
            format!("{sign}{} {}", v as u64, units[0])
        } else {
            // 1 dp; trim trailing ".0" for cleaner display.
            let formatted = format!("{:.1}", v);
            let trimmed = formatted.trim_end_matches(".0");
            format!("{sign}{trimmed} {}", units[i])
        }
    }

    /// Parse a humanized size string  bytes. Accepts "1.5KB", "1.5 KiB",
    /// "100B", "2 GB". Case-insensitive on the unit. Returns None on
    /// malformed input or unknown unit.
    fn parse_bytes(s: &str) -> Option<u64> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        // Split into numeric prefix + unit suffix.
        let split = t.find(|c: char| c.is_ascii_alphabetic())?;
        let (num_part, unit_part) = t.split_at(split);
        let value: f64 = num_part.trim().parse().ok()?;
        let mult = byte_unit_factor(unit_part.trim())?;
        Some((value * mult) as u64)
    }

    fn byte_unit_factor(u: &str) -> Option<f64> {
        let n = u.to_ascii_lowercase();
        Some(match n.as_str() {
            "b" | "byte" | "bytes" => 1.0,
            "kb" => 1e3, "mb" => 1e6, "gb" => 1e9,
            "tb" => 1e12, "pb" => 1e15, "eb" => 1e18,
            "kib" | "k" => 1024.0,
            "mib" | "m" => 1024.0 * 1024.0,
            "gib" | "g" => 1024.0 * 1024.0 * 1024.0,
            "tib" => 1024.0_f64.powi(4),
            "pib" => 1024.0_f64.powi(5),
            "eib" => 1024.0_f64.powi(6),
            _ => return None,
        })
    }

    /// Format seconds as human duration. Picks the 1-2 largest non-zero
    /// units. "90"  "1m 30s", "3700"  "1h 1m", "86460"  "1d 1m".
    fn format_duration(secs: i64) -> String {
        if secs == 0 {
            return "0s".to_string();
        }
        let sign = if secs < 0 { "-" } else { "" };
        let s = secs.unsigned_abs();
        let d = s / 86400;
        let h = (s % 86400) / 3600;
        let m = (s % 3600) / 60;
        let sec = s % 60;
        let mut parts: Vec<String> = alloc::vec![];
        if d > 0 { parts.push(format!("{d}d")); }
        if h > 0 { parts.push(format!("{h}h")); }
        if m > 0 { parts.push(format!("{m}m")); }
        if sec > 0 && d == 0 { parts.push(format!("{sec}s")); }
        // Cap at 2 most-significant units  "1d 5h" not "1d 5h 23m 7s".
        parts.truncate(2);
        format!("{sign}{}", parts.join(" "))
    }

    /// Parse "1h 30m" / "90s" / "1d 2h"  total seconds. Allows mixed
    /// case, optional whitespace. Returns None on garbage.
    fn parse_duration(s: &str) -> Option<i64> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        let mut total: f64 = 0.0;
        let mut current = String::new();
        let mut any = false;
        for c in t.chars() {
            if c.is_ascii_digit() || c == '.' || c == '-' {
                current.push(c);
            } else if c.is_ascii_alphabetic() {
                if current.is_empty() {
                    return None;
                }
                let value: f64 = current.parse().ok()?;
                let mult = match c.to_ascii_lowercase() {
                    's' => 1.0,
                    'm' => 60.0,
                    'h' => 3600.0,
                    'd' => 86400.0,
                    'w' => 604800.0,
                    'y' => 31557600.0,
                    _ => return None,
                };
                total += value * mult;
                current.clear();
                any = true;
            } else if c.is_whitespace() {
                // separator OK between [val][unit] tokens
            } else {
                return None;
            }
        }
        if !current.is_empty() { return None; }  // trailing number, no unit
        if !any { return None; }
        Some(total as i64)
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
                name: "humansize".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_BYTES, "humansize_bytes", 1, det),
                    s(FID_IBYTES, "humansize_ibytes", 1, det),
                    s(FID_PARSE_BYTES, "humansize_parse_bytes", 1, det),
                    s(FID_DURATION, "humansize_duration", 1, det),
                    s(FID_PARSE_DURATION, "humansize_parse_duration", 1, det),
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
                FID_BYTES => {
                    let n = arg_real(&args, 0, "humansize_bytes")?;
                    Ok(SqlValue::Text(format_bytes(n, false)))
                }
                FID_IBYTES => {
                    let n = arg_real(&args, 0, "humansize_ibytes")?;
                    Ok(SqlValue::Text(format_bytes(n, true)))
                }
                FID_PARSE_BYTES => {
                    let s = arg_text(&args, 0, "humansize_parse_bytes")?;
                    Ok(parse_bytes(&s)
                        .map(|n| SqlValue::Integer(n as i64))
                        .unwrap_or(SqlValue::Null))
                }
                FID_DURATION => {
                    let n = arg_real(&args, 0, "humansize_duration")?;
                    Ok(SqlValue::Text(format_duration(n as i64)))
                }
                FID_PARSE_DURATION => {
                    let s = arg_text(&args, 0, "humansize_parse_duration")?;
                    Ok(parse_duration(&s)
                        .map(SqlValue::Integer)
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("humansize: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
