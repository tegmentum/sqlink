//! heading degrees  cardinal direction

extern crate alloc;

#[cfg(target_arch = "wasm32")]
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

    const FID_CARDINAL_8: u64 = 1;
    const FID_CARDINAL_16: u64 = 2;
    const FID_DEGREES: u64 = 3;
    const FID_DISTANCE: u64 = 4;
    const FID_NORMALIZE: u64 = 5;

    struct Ext;

    /// 16-point compass: every 22.5°. Index 0 = N (000°), wrapping.
    /// Center degree of band i = i * 22.5; band span = [center-11.25,
    /// center+11.25).
    const POINTS_16: &[&str] = &[
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE",
        "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW",
    ];

    /// 8-point subset (every other index of POINTS_16).
    const POINTS_8: &[&str] = &[
        "N", "NE", "E", "SE", "S", "SW", "W", "NW",
    ];

    /// Wrap any degree value to [0, 360).
    fn normalize(deg: f64) -> f64 {
        ((deg % 360.0) + 360.0) % 360.0
    }

    /// Bucket a 0..360 degree into an N-point cardinal index.
    /// n is the number of points (8 or 16).
    fn bucket(deg: f64, n: usize) -> usize {
        let band = 360.0 / n as f64;
        let half = band / 2.0;
        let shifted = normalize(deg + half);
        let idx = (shifted / band) as usize % n;
        idx
    }

    fn cardinal_8(deg: f64) -> &'static str {
        POINTS_8[bucket(deg, 8)]
    }

    fn cardinal_16(deg: f64) -> &'static str {
        POINTS_16[bucket(deg, 16)]
    }

    /// Reverse: cardinal name (any of the 16 points)  center degree.
    /// Case-insensitive.
    fn degrees_for(name: &str) -> Option<f64> {
        let n = name.trim().to_ascii_uppercase();
        POINTS_16.iter().position(|p| **p == n).map(|i| {
            (i as f64) * 22.5
        })
    }

    /// Shortest angular distance between two bearings, in 0..180.
    /// d(0, 350) = 10, not 350.
    fn distance(a: f64, b: f64) -> f64 {
        let diff = (normalize(a) - normalize(b)).abs();
        if diff > 180.0 { 360.0 - diff } else { diff }
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
                name: "compass".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_CARDINAL_8, "compass_cardinal", 1, det),
                    s(FID_CARDINAL_16, "compass_cardinal16", 1, det),
                    s(FID_DEGREES, "compass_degrees", 1, det),
                    s(FID_DISTANCE, "compass_distance", 2, det),
                    s(FID_NORMALIZE, "compass_normalize", 1, det),
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
                FID_CARDINAL_8 => {
                    let d = arg_real(&args, 0, "compass_cardinal")?;
                    Ok(SqlValue::Text(cardinal_8(d).to_string()))
                }
                FID_CARDINAL_16 => {
                    let d = arg_real(&args, 0, "compass_cardinal16")?;
                    Ok(SqlValue::Text(cardinal_16(d).to_string()))
                }
                FID_DEGREES => {
                    let s = arg_text(&args, 0, "compass_degrees")?;
                    Ok(degrees_for(&s)
                        .map(SqlValue::Real)
                        .unwrap_or(SqlValue::Null))
                }
                FID_DISTANCE => {
                    let a = arg_real(&args, 0, "compass_distance")?;
                    let b = arg_real(&args, 1, "compass_distance")?;
                    Ok(SqlValue::Real(distance(a, b)))
                }
                FID_NORMALIZE => {
                    let d = arg_real(&args, 0, "compass_normalize")?;
                    Ok(SqlValue::Real(normalize(d)))
                }
                other => Err(format!("compass: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
