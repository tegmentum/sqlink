//! Lat/lon format conversion: decimal, DMS, DDM

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

    const FID_TO_DMS: u64 = 1;
    const FID_TO_DDM: u64 = 2;
    const FID_FROM_DMS: u64 = 3;
    const FID_NORMALIZE_LON: u64 = 4;
    const FID_NORMALIZE_LAT: u64 = 5;

    struct Ext;

    /// 'lat' or 'lon' picks the hemispheric suffix letter. Anything else
    /// returns None (caller gets NULL).
    fn hemi(axis: &str, value: f64) -> Option<char> {
        match axis.trim().to_ascii_lowercase().as_str() {
            "lat" | "latitude" => Some(if value >= 0.0 { 'N' } else { 'S' }),
            "lon" | "long" | "longitude" => Some(if value >= 0.0 { 'E' } else { 'W' }),
            _ => None,
        }
    }

    /// Decimal degrees  "DD MM SS" h. Seconds rounded to 0.01.
    fn to_dms(decimal: f64, axis: &str) -> Option<String> {
        let h = hemi(axis, decimal)?;
        let abs = decimal.abs();
        let deg = abs.trunc() as i32;
        let mfrac = (abs - deg as f64) * 60.0;
        let min = mfrac.trunc() as i32;
        let sec = (mfrac - min as f64) * 60.0;
        // Round to 2 decimal places without floating drift in display.
        let sec_str = format!("{:.2}", sec);
        Some(format!("{deg}° {min}' {sec_str}\" {h}"))
    }

    /// Decimal degrees  "DD MM.MMM" h (marine / aviation form).
    fn to_ddm(decimal: f64, axis: &str) -> Option<String> {
        let h = hemi(axis, decimal)?;
        let abs = decimal.abs();
        let deg = abs.trunc() as i32;
        let min = (abs - deg as f64) * 60.0;
        let min_str = format!("{:.3}", min);
        Some(format!("{deg}° {min_str}' {h}"))
    }

    /// Parse "40 42 46 N", "40°42'46\"N", "40 42 46 N", etc.
    /// decimal degrees. Returns None on garbage.
    /// Strategy: extract up to 3 numeric tokens + 1 hemisphere letter,
    /// recombine as dd + mm/60 + ss/3600 with sign from hemisphere.
    fn from_dms(s: &str) -> Option<f64> {
        let mut nums: Vec<f64> = alloc::vec![];
        let mut hemi: Option<char> = None;
        let mut current = String::new();
        for c in s.chars() {
            if c.is_ascii_digit() || c == '.' || c == '-' {
                current.push(c);
            } else {
                if !current.is_empty() {
                    if let Ok(n) = current.parse::<f64>() { nums.push(n); }
                    current.clear();
                }
                if c.is_ascii_alphabetic() {
                    let up = c.to_ascii_uppercase();
                    if matches!(up, 'N' | 'S' | 'E' | 'W') {
                        hemi = Some(up);
                    }
                }
            }
        }
        if !current.is_empty() {
            if let Ok(n) = current.parse::<f64>() { nums.push(n); }
        }
        if nums.is_empty() || nums.len() > 3 {
            return None;
        }
        let mut dd = nums[0];
        if nums.len() > 1 { dd += nums[1] / 60.0; }
        if nums.len() > 2 { dd += nums[2] / 3600.0; }
        // Hemisphere applies sign. If absent, retain whatever sign
        // was given on the degrees part (so "-40" stays negative).
        match hemi {
            Some('S') | Some('W') => Some(-dd.abs()),
            Some('N') | Some('E') => Some(dd.abs()),
            _ => Some(dd),  // raw signed value
        }
    }

    /// Wrap longitude to [-180, 180]. Uses Euclidean remainder so
    /// `180.0`  `-180.0` (boundary normalized to the negative side
    /// to match the conventional [-180, 180) half-open interval).
    fn normalize_lon(x: f64) -> f64 {
        let r = ((x + 180.0) % 360.0 + 360.0) % 360.0 - 180.0;
        if r == 180.0 { -180.0 } else { r }
    }

    /// Clamp latitude to [-90, 90]. Latitudes do NOT wrap  there's
    /// no "100° N" that means "10° S the other way." Clamp to pole.
    fn normalize_lat(x: f64) -> f64 {
        x.clamp(-90.0, 90.0)
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
                name: "latlon".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TO_DMS, "latlon_to_dms", 2, det),
                    s(FID_TO_DDM, "latlon_to_ddm", 2, det),
                    s(FID_FROM_DMS, "latlon_from_dms", 1, det),
                    s(FID_NORMALIZE_LON, "latlon_normalize_lon", 1, det),
                    s(FID_NORMALIZE_LAT, "latlon_normalize_lat", 1, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
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
                FID_TO_DMS => {
                    let v = arg_real(&args, 0, "latlon_to_dms")?;
                    let a = arg_text(&args, 1, "latlon_to_dms")?;
                    Ok(to_dms(v, &a).map(SqlValue::Text).unwrap_or(SqlValue::Null))
                }
                FID_TO_DDM => {
                    let v = arg_real(&args, 0, "latlon_to_ddm")?;
                    let a = arg_text(&args, 1, "latlon_to_ddm")?;
                    Ok(to_ddm(v, &a).map(SqlValue::Text).unwrap_or(SqlValue::Null))
                }
                FID_FROM_DMS => {
                    let s = arg_text(&args, 0, "latlon_from_dms")?;
                    Ok(from_dms(&s).map(SqlValue::Real).unwrap_or(SqlValue::Null))
                }
                FID_NORMALIZE_LON => {
                    let v = arg_real(&args, 0, "latlon_normalize_lon")?;
                    Ok(SqlValue::Real(normalize_lon(v)))
                }
                FID_NORMALIZE_LAT => {
                    let v = arg_real(&args, 0, "latlon_normalize_lat")?;
                    Ok(SqlValue::Real(normalize_lat(v)))
                }
                other => Err(format!("latlon: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
