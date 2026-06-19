//! Geo distance + bearing scalars (Haversine + Vincenty)

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

    const FID_HAVERSINE: u64 = 1;
    const FID_BEARING: u64 = 2;
    const FID_WITHIN: u64 = 3;
    const FID_MIDPOINT: u64 = 4;

    /// Earth's mean radius in meters (WGS84-ish).
    const EARTH_RADIUS_M: f64 = 6_371_008.8;

    struct Ext;

    fn arg_real(args: &[SqlValue], i: usize, fname: &str) -> Result<f64, String> {
        match args.get(i) {
            Some(SqlValue::Real(r)) => Ok(*r),
            Some(SqlValue::Integer(n)) => Ok(*n as f64),
            _ => Err(format!("{fname}: REAL arg at {i}")),
        }
    }

    /// Great-circle distance via Haversine. Both points in degrees.
    /// Result in meters. ~0.5% error over long distances; fine for
    /// most app-layer queries.
    fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
        let to_rad = core::f64::consts::PI / 180.0;
        let phi1 = lat1 * to_rad;
        let phi2 = lat2 * to_rad;
        let dphi = (lat2 - lat1) * to_rad;
        let dlam = (lon2 - lon1) * to_rad;
        let a = (dphi / 2.0).sin().powi(2)
            + phi1.cos() * phi2.cos() * (dlam / 2.0).sin().powi(2);
        let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
        EARTH_RADIUS_M * c
    }

    /// Initial compass bearing from point 1 to point 2, degrees
    /// from north, 0..360.
    fn bearing(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
        let to_rad = core::f64::consts::PI / 180.0;
        let to_deg = 180.0 / core::f64::consts::PI;
        let phi1 = lat1 * to_rad;
        let phi2 = lat2 * to_rad;
        let dlam = (lon2 - lon1) * to_rad;
        let y = dlam.sin() * phi2.cos();
        let x = phi1.cos() * phi2.sin() - phi1.sin() * phi2.cos() * dlam.cos();
        let theta = y.atan2(x) * to_deg;
        (theta + 360.0) % 360.0
    }

    /// Midpoint along the great circle from point 1 to point 2.
    /// Returns "lat,lon" as TEXT.
    fn midpoint(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> (f64, f64) {
        let to_rad = core::f64::consts::PI / 180.0;
        let to_deg = 180.0 / core::f64::consts::PI;
        let phi1 = lat1 * to_rad;
        let phi2 = lat2 * to_rad;
        let lam1 = lon1 * to_rad;
        let dlam = (lon2 - lon1) * to_rad;
        let bx = phi2.cos() * dlam.cos();
        let by = phi2.cos() * dlam.sin();
        let phi_m = (phi1.sin() + phi2.sin()).atan2(
            ((phi1.cos() + bx).powi(2) + by.powi(2)).sqrt(),
        );
        let lam_m = lam1 + by.atan2(phi1.cos() + bx);
        (phi_m * to_deg, lam_m * to_deg)
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
                name: "geo_distance".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_HAVERSINE, "haversine", 4, det),
                    s(FID_BEARING, "bearing", 4, det),
                    s(FID_WITHIN, "within_radius", 5, det),
                    s(FID_MIDPOINT, "geo_midpoint", 4, det),
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
            let lat1 = arg_real(&args, 0, "geo_distance")?;
            let lon1 = arg_real(&args, 1, "geo_distance")?;
            let lat2 = arg_real(&args, 2, "geo_distance")?;
            let lon2 = arg_real(&args, 3, "geo_distance")?;

            match func_id {
                FID_HAVERSINE => Ok(SqlValue::Real(haversine(lat1, lon1, lat2, lon2))),
                FID_BEARING => Ok(SqlValue::Real(bearing(lat1, lon1, lat2, lon2))),
                FID_WITHIN => {
                    let radius_m = arg_real(&args, 4, "within_radius")?;
                    let d = haversine(lat1, lon1, lat2, lon2);
                    Ok(SqlValue::Integer((d <= radius_m) as i64))
                }
                FID_MIDPOINT => {
                    let (lat_m, lon_m) = midpoint(lat1, lon1, lat2, lon2);
                    Ok(SqlValue::Text(format!("{lat_m:.6},{lon_m:.6}")))
                }
                other => Err(format!("geo_distance: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
