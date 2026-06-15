//! PostGIS bridge: routes SQLite scalar calls into postgis-wasm.
//!
//! Geometry crosses the boundary as BLOB containing WKB. Each
//! call reconstitutes the postgis-wasm `geometry` resource from
//! WKB at the boundary, performs the op, and materializes a
//! WKB BLOB on the way back when the result is itself a
//! geometry.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "bridge",
        generate_all,
    });
}

use bindings::exports::sqlite::extension::metadata::{
    Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
};
use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

use bindings::postgis::wasm::postgis_accessors as pg_acc;
use bindings::postgis::wasm::postgis_constructors as pg_ctor;
use bindings::postgis::wasm::postgis_measurements as pg_meas;
use bindings::postgis::wasm::postgis_predicates as pg_pred;
use bindings::postgis::wasm::postgis_types::Geometry;

// Function ids. Append-only across releases.
const FID_ST_MAKEPOINT: u64 = 1;
const FID_ST_GEOMFROMTEXT: u64 = 2;
const FID_ST_GEOMFROMWKB: u64 = 3;
const FID_ST_ASTEXT: u64 = 4;
const FID_ST_ASBINARY: u64 = 5;
const FID_ST_X: u64 = 6;
const FID_ST_Y: u64 = 7;
const FID_ST_DISTANCE: u64 = 8;
const FID_ST_AREA: u64 = 9;
const FID_ST_LENGTH: u64 = 10;
const FID_ST_INTERSECTS: u64 = 11;
const FID_ST_CONTAINS: u64 = 12;

struct PostgisBridge;

impl MetadataGuest for PostgisBridge {
    fn describe() -> Manifest {
        let det = FunctionFlags::DETERMINISTIC;
        let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
            id,
            name: name.into(),
            num_args,
            func_flags: det,
        };
        Manifest {
            name: "postgis".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            scalar_functions: alloc::vec![
                s(FID_ST_MAKEPOINT, "st_makepoint", 2),
                s(FID_ST_GEOMFROMTEXT, "st_geomfromtext", 1),
                s(FID_ST_GEOMFROMWKB, "st_geomfromwkb", 1),
                s(FID_ST_ASTEXT, "st_astext", 1),
                s(FID_ST_ASBINARY, "st_asbinary", 1),
                s(FID_ST_X, "st_x", 1),
                s(FID_ST_Y, "st_y", 1),
                s(FID_ST_DISTANCE, "st_distance", 2),
                s(FID_ST_AREA, "st_area", 1),
                s(FID_ST_LENGTH, "st_length", 1),
                s(FID_ST_INTERSECTS, "st_intersects", 2),
                s(FID_ST_CONTAINS, "st_contains", 2),
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

fn arg_f64(args: &[SqlValue], idx: usize, name: &str) -> Result<f64, String> {
    match args.get(idx) {
        Some(SqlValue::Integer(i)) => Ok(*i as f64),
        Some(SqlValue::Real(r)) => Ok(*r),
        Some(SqlValue::Text(s)) => s
            .parse::<f64>()
            .map_err(|_| format!("{name}: arg {idx} not numeric")),
        Some(_) => Err(format!("{name}: arg {idx} not numeric")),
        None => Err(format!("{name}: missing arg {idx}")),
    }
}

fn arg_text<'a>(args: &'a [SqlValue], idx: usize, name: &str) -> Result<&'a str, String> {
    match args.get(idx) {
        Some(SqlValue::Text(s)) => Ok(s.as_str()),
        _ => Err(format!("{name}: arg {idx} must be TEXT")),
    }
}

fn arg_blob<'a>(args: &'a [SqlValue], idx: usize, name: &str) -> Result<&'a [u8], String> {
    match args.get(idx) {
        Some(SqlValue::Blob(b)) => Ok(b.as_slice()),
        Some(SqlValue::Text(s)) => Ok(s.as_bytes()),
        _ => Err(format!("{name}: arg {idx} must be BLOB")),
    }
}

fn from_wkb(bytes: &[u8], name: &str) -> Result<Geometry, String> {
    Geometry::from_wkb(bytes).map_err(|e| format!("{name}: {}", postgis_err_string(e)))
}

fn postgis_err_string(e: bindings::postgis::wasm::postgis_types::PostgisError) -> String {
    use bindings::postgis::wasm::postgis_types::PostgisError as E;
    match e {
        E::InvalidGeometry(s) => format!("invalid geometry: {s}"),
        E::ParseError(s) => format!("parse error: {s}"),
        E::UnsupportedOperation(s) => format!("unsupported: {s}"),
        E::NumericError(s) => format!("numeric: {s}"),
        E::SridMismatch(s) => format!("SRID mismatch: {s}"),
        E::General(s) => s,
    }
}

impl ScalarFunctionGuest for PostgisBridge {
    fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
        // SQLite NULL propagation across the spatial ops.
        if args.iter().any(|v| matches!(v, SqlValue::Null)) {
            return Ok(SqlValue::Null);
        }
        match func_id {
            FID_ST_MAKEPOINT => {
                let x = arg_f64(&args, 0, "st_makepoint")?;
                let y = arg_f64(&args, 1, "st_makepoint")?;
                let g = pg_ctor::st_make_point(x, y);
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMTEXT => {
                let wkt = arg_text(&args, 0, "st_geomfromtext")?;
                let g = pg_ctor::st_geom_from_text(wkt)
                    .map_err(|e| format!("st_geomfromtext: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMWKB => {
                let wkb = arg_blob(&args, 0, "st_geomfromwkb")?;
                let g = from_wkb(wkb, "st_geomfromwkb")?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_ASTEXT => {
                let g = from_wkb(arg_blob(&args, 0, "st_astext")?, "st_astext")?;
                Ok(SqlValue::Text(g.as_wkt()))
            }
            FID_ST_ASBINARY => {
                let g = from_wkb(arg_blob(&args, 0, "st_asbinary")?, "st_asbinary")?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_X => {
                let g = from_wkb(arg_blob(&args, 0, "st_x")?, "st_x")?;
                let x = pg_acc::st_x(&g)
                    .map_err(|e| format!("st_x: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Real(x))
            }
            FID_ST_Y => {
                let g = from_wkb(arg_blob(&args, 0, "st_y")?, "st_y")?;
                let y = pg_acc::st_y(&g)
                    .map_err(|e| format!("st_y: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Real(y))
            }
            FID_ST_DISTANCE => {
                let a = from_wkb(arg_blob(&args, 0, "st_distance")?, "st_distance")?;
                let b = from_wkb(arg_blob(&args, 1, "st_distance")?, "st_distance")?;
                let d = pg_meas::st_distance(&a, &b)
                    .map_err(|e| format!("st_distance: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Real(d))
            }
            FID_ST_AREA => {
                let g = from_wkb(arg_blob(&args, 0, "st_area")?, "st_area")?;
                let a = pg_meas::st_area(&g)
                    .map_err(|e| format!("st_area: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Real(a))
            }
            FID_ST_LENGTH => {
                let g = from_wkb(arg_blob(&args, 0, "st_length")?, "st_length")?;
                let l = pg_meas::st_length(&g)
                    .map_err(|e| format!("st_length: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Real(l))
            }
            FID_ST_INTERSECTS => {
                let a = from_wkb(arg_blob(&args, 0, "st_intersects")?, "st_intersects")?;
                let b = from_wkb(arg_blob(&args, 1, "st_intersects")?, "st_intersects")?;
                let r = pg_pred::st_intersects(&a, &b)
                    .map_err(|e| format!("st_intersects: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Integer(r as i64))
            }
            FID_ST_CONTAINS => {
                let a = from_wkb(arg_blob(&args, 0, "st_contains")?, "st_contains")?;
                let b = from_wkb(arg_blob(&args, 1, "st_contains")?, "st_contains")?;
                let r = pg_pred::st_contains(&a, &b)
                    .map_err(|e| format!("st_contains: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Integer(r as i64))
            }
            other => Err(format!("postgis bridge: unknown func id {other}")),
        }
    }
}

bindings::export!(PostgisBridge with_types_in bindings);
