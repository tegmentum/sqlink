//! Embed path for geo-distance. All FFI glue is in `sqlite-embed`;
//! this is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_HAVERSINE: u64 = 1;
const FID_BEARING:   u64 = 2;
const FID_WITHIN:    u64 = 3;
const FID_MIDPOINT:  u64 = 4;

/// Earth's mean radius in meters (WGS84-ish).
const EARTH_RADIUS_M: f64 = 6_371_008.8;

fn arg_real(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<f64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Real(r)) => Ok(*r),
        Some(SqlValueOwned::Integer(n)) => Ok(*n as f64),
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

/// Initial compass bearing from point 1 to point 2, degrees from
/// north, 0..360.
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

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let lat1 = arg_real(&args, 0, "geo_distance")?;
    let lon1 = arg_real(&args, 1, "geo_distance")?;
    let lat2 = arg_real(&args, 2, "geo_distance")?;
    let lon2 = arg_real(&args, 3, "geo_distance")?;

    match func_id {
        FID_HAVERSINE => Ok(SqlValueOwned::Real(haversine(lat1, lon1, lat2, lon2))),
        FID_BEARING => Ok(SqlValueOwned::Real(bearing(lat1, lon1, lat2, lon2))),
        FID_WITHIN => {
            let radius_m = arg_real(&args, 4, "within_radius")?;
            let d = haversine(lat1, lon1, lat2, lon2);
            Ok(SqlValueOwned::Integer((d <= radius_m) as i64))
        }
        FID_MIDPOINT => {
            let (lat_m, lon_m) = midpoint(lat1, lon1, lat2, lon2);
            Ok(SqlValueOwned::Text(format!("{lat_m:.6},{lon_m:.6}")))
        }
        other => Err(format!("geo_distance: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_HAVERSINE, name: b"haversine\0",     num_args: 4, deterministic: true },
    ScalarSpec { func_id: FID_BEARING,   name: b"bearing\0",       num_args: 4, deterministic: true },
    ScalarSpec { func_id: FID_WITHIN,    name: b"within_radius\0", num_args: 5, deterministic: true },
    ScalarSpec { func_id: FID_MIDPOINT,  name: b"geo_midpoint\0",  num_args: 4, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
