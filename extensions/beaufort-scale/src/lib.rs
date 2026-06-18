//! Beaufort wind scale m/s  named force

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

    const FID_FORCE: u64 = 1;
    const FID_NAME: u64 = 2;
    const FID_FROM_KMH: u64 = 3;
    const FID_FROM_MPH: u64 = 4;
    const FID_MIN_MS: u64 = 5;

    struct Ext;

    /// (force, lower bound m/s, name). Upper bound is implicit:
    /// next entry's lower bound. Force 12 is open-ended.
    /// Source: WMO Beaufort scale, 10 m above ground.
    const TABLE: &[(u8, f64, &str)] = &[
        ( 0,  0.0, "Calm"),
        ( 1,  0.5, "Light air"),
        ( 2,  1.6, "Light breeze"),
        ( 3,  3.4, "Gentle breeze"),
        ( 4,  5.5, "Moderate breeze"),
        ( 5,  8.0, "Fresh breeze"),
        ( 6, 10.8, "Strong breeze"),
        ( 7, 13.9, "High wind"),
        ( 8, 17.2, "Gale"),
        ( 9, 20.8, "Strong gale"),
        (10, 24.5, "Storm"),
        (11, 28.5, "Violent storm"),
        (12, 32.7, "Hurricane"),
    ];

    /// Bucket m/s  Beaufort force. Walk from the top so the open-
    /// ended 12 catches anything beyond 32.7; tie-breaks on the
    /// lower bound go to the lower force (matches WMO convention).
    fn force_for(ms: f64) -> u8 {
        if ms < 0.0 {
            return 0;
        }
        for (force, lower, _) in TABLE.iter().rev() {
            if ms >= *lower {
                return *force;
            }
        }
        0
    }

    fn name_for(force: u8) -> Option<&'static str> {
        TABLE.iter().find(|(f, _, _)| *f == force).map(|(_, _, n)| *n)
    }

    fn min_ms_for(force: u8) -> Option<f64> {
        TABLE.iter().find(|(f, _, _)| *f == force).map(|(_, m, _)| *m)
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
                name: "beaufort_scale".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FORCE, "beaufort_force", 1, det),
                    s(FID_NAME, "beaufort_name", 1, det),
                    s(FID_FROM_KMH, "beaufort_from_kmh", 1, det),
                    s(FID_FROM_MPH, "beaufort_from_mph", 1, det),
                    s(FID_MIN_MS, "beaufort_min_ms", 1, det),
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
                FID_FORCE => {
                    let ms = arg_real(&args, 0, "beaufort_force")?;
                    Ok(SqlValue::Integer(force_for(ms) as i64))
                }
                FID_NAME => {
                    let ms = arg_real(&args, 0, "beaufort_name")?;
                    let f = force_for(ms);
                    Ok(name_for(f)
                        .map(|n| SqlValue::Text(n.to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                FID_FROM_KMH => {
                    let kmh = arg_real(&args, 0, "beaufort_from_kmh")?;
                    Ok(SqlValue::Integer(force_for(kmh / 3.6) as i64))
                }
                FID_FROM_MPH => {
                    let mph = arg_real(&args, 0, "beaufort_from_mph")?;
                    Ok(SqlValue::Integer(force_for(mph * 0.44704) as i64))
                }
                FID_MIN_MS => {
                    let f = arg_int(&args, 0, "beaufort_min_ms")? as u8;
                    Ok(min_ms_for(f)
                        .map(SqlValue::Real)
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("beaufort: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
