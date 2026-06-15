//! PostGIS bridge: routes SQLite scalar calls into postgis-wasm.
//!
//! Geometry crosses the boundary as BLOB containing WKB. Each
//! call reconstitutes the postgis-wasm `geometry` resource from
//! WKB at the boundary, performs the op, and materializes a
//! WKB BLOB on the way back when the result is itself a
//! geometry.
//!
//! The dispatch surface is large (~110 functions) but mostly
//! pattern-matched: macros below collapse the boilerplate so
//! adding the next batch of postgis-wasm exports is one line of
//! manifest + one line of dispatch each.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
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
use bindings::postgis::wasm::postgis_output as pg_out;
use bindings::postgis::wasm::postgis_predicates as pg_pred;
use bindings::postgis::wasm::postgis_processing as pg_proc;
use bindings::postgis::wasm::postgis_transformations as pg_xform;
use bindings::postgis::wasm::postgis_types::Geometry;

// Function ids. Append-only. Ranges by category to leave space:
//   1..50    constructors
//   50..100  accessors
//   100..150 measurements
//   150..200 predicates
//   200..250 processing
//   250..300 output

// Constructors
const FID_ST_MAKEPOINT: u64 = 1;
const FID_ST_MAKEPOINT_Z: u64 = 2;
const FID_ST_MAKEPOINT_M: u64 = 3;
const FID_ST_MAKEPOINT_ZM: u64 = 4;
const FID_ST_POINT: u64 = 5;
const FID_ST_POINT_Z: u64 = 6;
const FID_ST_POINT_M: u64 = 7;
const FID_ST_POINT_ZM: u64 = 8;
const FID_ST_MAKE_ENVELOPE: u64 = 9;
const FID_ST_MAKE_ENVELOPE_SRID: u64 = 10;
const FID_ST_GEOMFROMTEXT: u64 = 11;
const FID_ST_GEOMFROMTEXT_SRID: u64 = 12;
const FID_ST_GEOMFROMEWKT: u64 = 13;
const FID_ST_POINTFROMTEXT: u64 = 14;
const FID_ST_GEOMFROMWKB: u64 = 15;
const FID_ST_GEOMFROMGEOJSON: u64 = 16;
const FID_ST_MAKE_LINE_TWO: u64 = 17;

// Accessors
const FID_ST_X: u64 = 50;
const FID_ST_Y: u64 = 51;
const FID_ST_XMIN: u64 = 52;
const FID_ST_XMAX: u64 = 53;
const FID_ST_YMIN: u64 = 54;
const FID_ST_YMAX: u64 = 55;
const FID_ST_SRID: u64 = 56;
const FID_ST_GEOMETRY_TYPE: u64 = 57;
const FID_ST_IS_EMPTY: u64 = 58;
const FID_ST_IS_VALID: u64 = 59;
const FID_ST_IS_SIMPLE: u64 = 60;
const FID_ST_IS_CLOSED: u64 = 61;
const FID_ST_IS_RING: u64 = 62;
const FID_ST_NUM_POINTS: u64 = 63;
const FID_ST_NUM_GEOMETRIES: u64 = 64;
const FID_ST_NUM_INTERIOR_RINGS: u64 = 65;
const FID_ST_NPOINTS: u64 = 66;
const FID_ST_EXTERIOR_RING: u64 = 67;
const FID_ST_INTERIOR_RING_N: u64 = 68;
const FID_ST_POINT_N: u64 = 69;
const FID_ST_GEOMETRY_N: u64 = 70;
const FID_ST_START_POINT: u64 = 71;
const FID_ST_END_POINT: u64 = 72;
const FID_ST_BOUNDARY: u64 = 73;
const FID_ST_ENVELOPE: u64 = 74;
const FID_ST_SET_SRID: u64 = 75;

// Measurements
const FID_ST_AREA: u64 = 100;
const FID_ST_LENGTH: u64 = 101;
const FID_ST_PERIMETER: u64 = 102;
const FID_ST_LENGTH_TWOD: u64 = 103;
const FID_ST_LENGTH_THREED: u64 = 104;
const FID_ST_PERIMETER_THREED: u64 = 105;
const FID_ST_DISTANCE: u64 = 106;
const FID_ST_DISTANCE_THREED: u64 = 107;
const FID_ST_MAX_DISTANCE: u64 = 108;
const FID_ST_MAX_DISTANCE_THREED: u64 = 109;
const FID_ST_HAUSDORFF_DISTANCE: u64 = 110;
const FID_ST_FRECHET_DISTANCE: u64 = 111;

// Predicates
const FID_ST_INTERSECTS: u64 = 150;
const FID_ST_CONTAINS: u64 = 151;
const FID_ST_WITHIN: u64 = 152;
const FID_ST_EQUALS: u64 = 153;
const FID_ST_DISJOINT: u64 = 154;
const FID_ST_OVERLAPS: u64 = 155;
const FID_ST_TOUCHES: u64 = 156;
const FID_ST_CROSSES: u64 = 157;
const FID_ST_COVERED_BY: u64 = 158;
const FID_ST_COVERS: u64 = 159;
const FID_ST_CONTAINS_PROPERLY: u64 = 160;
const FID_ST_3D_INTERSECTS: u64 = 161;
const FID_ST_3D_DISJOINT: u64 = 162;

// Processing
const FID_ST_BUFFER: u64 = 200;
const FID_ST_INTERSECTION: u64 = 201;
const FID_ST_UNION: u64 = 202;
const FID_ST_DIFFERENCE: u64 = 203;
const FID_ST_SYM_DIFFERENCE: u64 = 204;
const FID_ST_UNARY_UNION: u64 = 205;
const FID_ST_SIMPLIFY: u64 = 206;
const FID_ST_SIMPLIFY_PT: u64 = 207;
const FID_ST_SIMPLIFY_VW: u64 = 208;
const FID_ST_CONVEX_HULL: u64 = 209;
const FID_ST_CONCAVE_HULL: u64 = 210;
const FID_ST_CENTROID: u64 = 211;
const FID_ST_POINT_ON_SURFACE: u64 = 212;
const FID_ST_ORIENTED_ENVELOPE: u64 = 213;
const FID_ST_MIN_BOUNDING_CIRCLE: u64 = 214;
const FID_ST_LINE_MERGE: u64 = 215;
const FID_ST_MAKE_VALID: u64 = 216;
const FID_ST_REVERSE: u64 = 217;
const FID_ST_FLIP_COORDINATES: u64 = 218;
const FID_ST_FORCE_2D: u64 = 219;
const FID_ST_FORCE_3D: u64 = 220;
const FID_ST_MULTI: u64 = 221;
const FID_ST_COLLECTION_HOMOGENIZE: u64 = 222;

// Output
const FID_ST_ASTEXT: u64 = 250;
const FID_ST_ASBINARY: u64 = 251;
const FID_ST_AS_EWKT: u64 = 252;
const FID_ST_AS_EWKB: u64 = 253;
const FID_ST_AS_HEXEWKB: u64 = 254;
const FID_ST_AS_GEOJSON: u64 = 255;
const FID_ST_AS_SVG: u64 = 256;
const FID_ST_AS_KML: u64 = 257;
const FID_ST_AS_GML: u64 = 258;
const FID_ST_AS_X3D: u64 = 259;
const FID_ST_SUMMARY: u64 = 260;
const FID_ST_GEOHASH: u64 = 261;

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
                // Constructors
                s(FID_ST_MAKEPOINT, "st_makepoint", 2),
                s(FID_ST_MAKEPOINT_Z, "st_makepointz", 3),
                s(FID_ST_MAKEPOINT_M, "st_makepointm", 3),
                s(FID_ST_MAKEPOINT_ZM, "st_makepointzm", 4),
                s(FID_ST_POINT, "st_point", 2),
                s(FID_ST_POINT_Z, "st_pointz", 3),
                s(FID_ST_POINT_M, "st_pointm", 3),
                s(FID_ST_POINT_ZM, "st_pointzm", 4),
                s(FID_ST_MAKE_ENVELOPE, "st_makeenvelope", 4),
                s(FID_ST_MAKE_ENVELOPE_SRID, "st_makeenvelope_srid", 5),
                s(FID_ST_GEOMFROMTEXT, "st_geomfromtext", 1),
                s(FID_ST_GEOMFROMTEXT_SRID, "st_geomfromtext_srid", 2),
                s(FID_ST_GEOMFROMEWKT, "st_geomfromewkt", 1),
                s(FID_ST_POINTFROMTEXT, "st_pointfromtext", 1),
                s(FID_ST_GEOMFROMWKB, "st_geomfromwkb", 1),
                s(FID_ST_GEOMFROMGEOJSON, "st_geomfromgeojson", 1),
                s(FID_ST_MAKE_LINE_TWO, "st_makeline", 2),
                // Accessors
                s(FID_ST_X, "st_x", 1),
                s(FID_ST_Y, "st_y", 1),
                s(FID_ST_XMIN, "st_xmin", 1),
                s(FID_ST_XMAX, "st_xmax", 1),
                s(FID_ST_YMIN, "st_ymin", 1),
                s(FID_ST_YMAX, "st_ymax", 1),
                s(FID_ST_SRID, "st_srid", 1),
                s(FID_ST_GEOMETRY_TYPE, "st_geometrytype", 1),
                s(FID_ST_IS_EMPTY, "st_isempty", 1),
                s(FID_ST_IS_VALID, "st_isvalid", 1),
                s(FID_ST_IS_SIMPLE, "st_issimple", 1),
                s(FID_ST_IS_CLOSED, "st_isclosed", 1),
                s(FID_ST_IS_RING, "st_isring", 1),
                s(FID_ST_NUM_POINTS, "st_numpoints", 1),
                s(FID_ST_NUM_GEOMETRIES, "st_numgeometries", 1),
                s(FID_ST_NUM_INTERIOR_RINGS, "st_numinteriorrings", 1),
                s(FID_ST_NPOINTS, "st_npoints", 1),
                s(FID_ST_EXTERIOR_RING, "st_exteriorring", 1),
                s(FID_ST_INTERIOR_RING_N, "st_interiorringn", 2),
                s(FID_ST_POINT_N, "st_pointn", 2),
                s(FID_ST_GEOMETRY_N, "st_geometryn", 2),
                s(FID_ST_START_POINT, "st_startpoint", 1),
                s(FID_ST_END_POINT, "st_endpoint", 1),
                s(FID_ST_BOUNDARY, "st_boundary", 1),
                s(FID_ST_ENVELOPE, "st_envelope", 1),
                s(FID_ST_SET_SRID, "st_setsrid", 2),
                // Measurements
                s(FID_ST_AREA, "st_area", 1),
                s(FID_ST_LENGTH, "st_length", 1),
                s(FID_ST_PERIMETER, "st_perimeter", 1),
                s(FID_ST_LENGTH_TWOD, "st_length2d", 1),
                s(FID_ST_LENGTH_THREED, "st_length3d", 1),
                s(FID_ST_PERIMETER_THREED, "st_perimeter3d", 1),
                s(FID_ST_DISTANCE, "st_distance", 2),
                s(FID_ST_DISTANCE_THREED, "st_distance3d", 2),
                s(FID_ST_MAX_DISTANCE, "st_maxdistance", 2),
                s(FID_ST_MAX_DISTANCE_THREED, "st_maxdistance3d", 2),
                s(FID_ST_HAUSDORFF_DISTANCE, "st_hausdorffdistance", 2),
                s(FID_ST_FRECHET_DISTANCE, "st_frechetdistance", 2),
                // Predicates
                s(FID_ST_INTERSECTS, "st_intersects", 2),
                s(FID_ST_CONTAINS, "st_contains", 2),
                s(FID_ST_WITHIN, "st_within", 2),
                s(FID_ST_EQUALS, "st_equals", 2),
                s(FID_ST_DISJOINT, "st_disjoint", 2),
                s(FID_ST_OVERLAPS, "st_overlaps", 2),
                s(FID_ST_TOUCHES, "st_touches", 2),
                s(FID_ST_CROSSES, "st_crosses", 2),
                s(FID_ST_COVERED_BY, "st_coveredby", 2),
                s(FID_ST_COVERS, "st_covers", 2),
                s(FID_ST_CONTAINS_PROPERLY, "st_containsproperly", 2),
                s(FID_ST_3D_INTERSECTS, "st_3dintersects", 2),
                s(FID_ST_3D_DISJOINT, "st_3ddisjoint", 2),
                // Processing
                s(FID_ST_BUFFER, "st_buffer", 2),
                s(FID_ST_INTERSECTION, "st_intersection", 2),
                s(FID_ST_UNION, "st_union", 2),
                s(FID_ST_DIFFERENCE, "st_difference", 2),
                s(FID_ST_SYM_DIFFERENCE, "st_symdifference", 2),
                s(FID_ST_UNARY_UNION, "st_unaryunion", 1),
                s(FID_ST_SIMPLIFY, "st_simplify", 2),
                s(FID_ST_SIMPLIFY_PT, "st_simplifypreservetopology", 2),
                s(FID_ST_SIMPLIFY_VW, "st_simplifyvw", 2),
                s(FID_ST_CONVEX_HULL, "st_convexhull", 1),
                s(FID_ST_CONCAVE_HULL, "st_concavehull", 2),
                s(FID_ST_CENTROID, "st_centroid", 1),
                s(FID_ST_POINT_ON_SURFACE, "st_pointonsurface", 1),
                s(FID_ST_ORIENTED_ENVELOPE, "st_orientedenvelope", 1),
                s(FID_ST_MIN_BOUNDING_CIRCLE, "st_minimumboundingcircle", 1),
                s(FID_ST_LINE_MERGE, "st_linemerge", 1),
                s(FID_ST_MAKE_VALID, "st_makevalid", 1),
                s(FID_ST_REVERSE, "st_reverse", 1),
                s(FID_ST_FLIP_COORDINATES, "st_flipcoordinates", 1),
                s(FID_ST_FORCE_2D, "st_force2d", 1),
                s(FID_ST_FORCE_3D, "st_force3d", 1),
                s(FID_ST_MULTI, "st_multi", 1),
                s(FID_ST_COLLECTION_HOMOGENIZE, "st_collectionhomogenize", 1),
                // Output
                s(FID_ST_ASTEXT, "st_astext", 1),
                s(FID_ST_ASBINARY, "st_asbinary", 1),
                s(FID_ST_AS_EWKT, "st_asewkt", 1),
                s(FID_ST_AS_EWKB, "st_asewkb", 1),
                s(FID_ST_AS_HEXEWKB, "st_ashexewkb", 1),
                s(FID_ST_AS_GEOJSON, "st_asgeojson", 1),
                s(FID_ST_AS_SVG, "st_assvg", 1),
                s(FID_ST_AS_KML, "st_askml", 1),
                s(FID_ST_AS_GML, "st_asgml", 1),
                s(FID_ST_AS_X3D, "st_asx3d", 1),
                s(FID_ST_SUMMARY, "st_summary", 1),
                s(FID_ST_GEOHASH, "st_geohash", 1),
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

// ───────────── Helpers ─────────────

fn arg_f64(args: &[SqlValue], idx: usize, name: &str) -> Result<f64, String> {
    match args.get(idx) {
        Some(SqlValue::Integer(i)) => Ok(*i as f64),
        Some(SqlValue::Real(r)) => Ok(*r),
        Some(SqlValue::Text(s)) => s
            .parse::<f64>()
            .map_err(|_| format!("{name}: arg {idx} not numeric")),
        _ => Err(format!("{name}: arg {idx} not numeric")),
    }
}

fn arg_i64(args: &[SqlValue], idx: usize, name: &str) -> Result<i64, String> {
    match args.get(idx) {
        Some(SqlValue::Integer(i)) => Ok(*i),
        Some(SqlValue::Real(r)) => Ok(*r as i64),
        _ => Err(format!("{name}: arg {idx} not integer")),
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

// ───────────── Dispatch macros ─────────────

/// f(geom) -> Result<f64>  most accessors / measurements
macro_rules! g_to_f64 {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let r = $module::$fn(&g)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Real(r))
    }};
}

/// f(geom) -> u32  infallible counts.
macro_rules! g_to_int {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Integer($module::$fn(&g) as i64))
    }};
}

/// f(geom) -> Result<u32>  fallible counts.
macro_rules! g_to_int_result {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let r = $module::$fn(&g)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Integer(r as i64))
    }};
}

/// f(geom) -> bool  infallible is-X predicates.
macro_rules! g_to_bool {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Integer($module::$fn(&g) as i64))
    }};
}

/// f(geom) -> string  as-text / as-geojson / etc.
macro_rules! g_to_string {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Text($module::$fn(&g)))
    }};
}

/// f(geom) -> Result<string>  fallible string outputs.
macro_rules! g_to_string_result {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let s = $module::$fn(&g)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Text(s))
    }};
}

/// f(geom) -> list<u8>  as-binary / as-ewkb.
macro_rules! g_to_blob {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Blob($module::$fn(&g)))
    }};
}

/// f(geom) -> Result<geometry>
macro_rules! g_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let r = $module::$fn(&g)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(r.as_wkb()))
    }};
}

/// f(geom) -> geometry  infallible.
macro_rules! g_to_geom_inf {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Blob($module::$fn(&g).as_wkb()))
    }};
}

/// f(geom1, geom2) -> Result<f64>
macro_rules! gg_to_f64 {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        let r = $module::$fn(&a, &b)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Real(r))
    }};
}

/// f(geom1, geom2) -> Result<bool>
macro_rules! gg_to_bool {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        let r = $module::$fn(&a, &b)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Integer(r as i64))
    }};
}

/// f(geom1, geom2) -> Result<geometry>
macro_rules! gg_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        let r = $module::$fn(&a, &b)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(r.as_wkb()))
    }};
}

/// f(geom, f64) -> Result<geometry>  buffer/simplify shape.
macro_rules! gd_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let d = arg_f64(&$args, 1, $name)?;
        let r = $module::$fn(&g, d)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(r.as_wkb()))
    }};
}

// ───────────── Dispatch ─────────────

impl ScalarFunctionGuest for PostgisBridge {
    fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
        if args.iter().any(|v| matches!(v, SqlValue::Null)) {
            return Ok(SqlValue::Null);
        }
        match func_id {
            // ── Constructors ──
            FID_ST_MAKEPOINT | FID_ST_POINT => {
                let x = arg_f64(&args, 0, "st_makepoint")?;
                let y = arg_f64(&args, 1, "st_makepoint")?;
                Ok(SqlValue::Blob(pg_ctor::st_make_point(x, y).as_wkb()))
            }
            FID_ST_MAKEPOINT_Z | FID_ST_POINT_Z => {
                let x = arg_f64(&args, 0, "st_makepointz")?;
                let y = arg_f64(&args, 1, "st_makepointz")?;
                let z = arg_f64(&args, 2, "st_makepointz")?;
                Ok(SqlValue::Blob(pg_ctor::st_make_point_z(x, y, z).as_wkb()))
            }
            FID_ST_MAKEPOINT_M | FID_ST_POINT_M => {
                let x = arg_f64(&args, 0, "st_makepointm")?;
                let y = arg_f64(&args, 1, "st_makepointm")?;
                let m = arg_f64(&args, 2, "st_makepointm")?;
                Ok(SqlValue::Blob(pg_ctor::st_make_point_m(x, y, m).as_wkb()))
            }
            FID_ST_MAKEPOINT_ZM | FID_ST_POINT_ZM => {
                let x = arg_f64(&args, 0, "st_makepointzm")?;
                let y = arg_f64(&args, 1, "st_makepointzm")?;
                let z = arg_f64(&args, 2, "st_makepointzm")?;
                let m = arg_f64(&args, 3, "st_makepointzm")?;
                Ok(SqlValue::Blob(pg_ctor::st_make_point_zm(x, y, z, m).as_wkb()))
            }
            FID_ST_MAKE_ENVELOPE => {
                let xmin = arg_f64(&args, 0, "st_makeenvelope")?;
                let ymin = arg_f64(&args, 1, "st_makeenvelope")?;
                let xmax = arg_f64(&args, 2, "st_makeenvelope")?;
                let ymax = arg_f64(&args, 3, "st_makeenvelope")?;
                Ok(SqlValue::Blob(
                    pg_ctor::st_make_envelope(xmin, ymin, xmax, ymax).as_wkb(),
                ))
            }
            FID_ST_MAKE_ENVELOPE_SRID => {
                let xmin = arg_f64(&args, 0, "st_makeenvelope_srid")?;
                let ymin = arg_f64(&args, 1, "st_makeenvelope_srid")?;
                let xmax = arg_f64(&args, 2, "st_makeenvelope_srid")?;
                let ymax = arg_f64(&args, 3, "st_makeenvelope_srid")?;
                let srid = arg_i64(&args, 4, "st_makeenvelope_srid")? as i32;
                Ok(SqlValue::Blob(
                    pg_ctor::st_make_envelope_srid(xmin, ymin, xmax, ymax, srid).as_wkb(),
                ))
            }
            FID_ST_GEOMFROMTEXT => {
                let wkt = arg_text(&args, 0, "st_geomfromtext")?;
                let g = pg_ctor::st_geom_from_text(wkt)
                    .map_err(|e| format!("st_geomfromtext: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMTEXT_SRID => {
                let wkt = arg_text(&args, 0, "st_geomfromtext_srid")?;
                let srid = arg_i64(&args, 1, "st_geomfromtext_srid")? as i32;
                let g = pg_ctor::st_geom_from_text_srid(wkt, srid)
                    .map_err(|e| format!("st_geomfromtext_srid: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMEWKT => {
                let wkt = arg_text(&args, 0, "st_geomfromewkt")?;
                let g = pg_ctor::st_geom_from_ewkt(wkt)
                    .map_err(|e| format!("st_geomfromewkt: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_POINTFROMTEXT => {
                let wkt = arg_text(&args, 0, "st_pointfromtext")?;
                let g = pg_ctor::st_point_from_text(wkt)
                    .map_err(|e| format!("st_pointfromtext: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMWKB => {
                let wkb = arg_blob(&args, 0, "st_geomfromwkb")?;
                let g = from_wkb(wkb, "st_geomfromwkb")?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMGEOJSON => {
                let s = arg_text(&args, 0, "st_geomfromgeojson")?;
                let g = Geometry::from_geojson(s)
                    .map_err(|e| format!("st_geomfromgeojson: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_MAKE_LINE_TWO => {
                let a = from_wkb(arg_blob(&args, 0, "st_makeline")?, "st_makeline")?;
                let b = from_wkb(arg_blob(&args, 1, "st_makeline")?, "st_makeline")?;
                let g = pg_ctor::st_make_line_two(&a, &b)
                    .map_err(|e| format!("st_makeline: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }

            // ── Accessors ──
            FID_ST_X => g_to_f64!(args, "st_x", pg_acc::st_x),
            FID_ST_Y => g_to_f64!(args, "st_y", pg_acc::st_y),
            FID_ST_XMIN => g_to_f64!(args, "st_xmin", pg_acc::st_xmin),
            FID_ST_XMAX => g_to_f64!(args, "st_xmax", pg_acc::st_xmax),
            FID_ST_YMIN => g_to_f64!(args, "st_ymin", pg_acc::st_ymin),
            FID_ST_YMAX => g_to_f64!(args, "st_ymax", pg_acc::st_ymax),
            FID_ST_SRID => {
                let g = from_wkb(arg_blob(&args, 0, "st_srid")?, "st_srid")?;
                Ok(match g.srid() {
                    Some(s) => SqlValue::Integer(s as i64),
                    None => SqlValue::Null,
                })
            }
            FID_ST_GEOMETRY_TYPE => {
                let g = from_wkb(arg_blob(&args, 0, "st_geometrytype")?, "st_geometrytype")?;
                let name = match g.geometry_type() {
                    bindings::postgis::wasm::postgis_types::GeometryType::Point => "POINT",
                    bindings::postgis::wasm::postgis_types::GeometryType::LineString => "LINESTRING",
                    bindings::postgis::wasm::postgis_types::GeometryType::Polygon => "POLYGON",
                    bindings::postgis::wasm::postgis_types::GeometryType::MultiPoint => "MULTIPOINT",
                    bindings::postgis::wasm::postgis_types::GeometryType::MultiLineString => "MULTILINESTRING",
                    bindings::postgis::wasm::postgis_types::GeometryType::MultiPolygon => "MULTIPOLYGON",
                    bindings::postgis::wasm::postgis_types::GeometryType::GeometryCollection => "GEOMETRYCOLLECTION",
                };
                Ok(SqlValue::Text(format!("ST_{name}").to_string()))
            }
            FID_ST_IS_EMPTY => {
                let g = from_wkb(arg_blob(&args, 0, "st_isempty")?, "st_isempty")?;
                Ok(SqlValue::Integer(g.is_empty() as i64))
            }
            FID_ST_IS_VALID => g_to_bool!(args, "st_isvalid", pg_pred::st_is_valid),
            FID_ST_IS_SIMPLE => g_to_bool!(args, "st_issimple", pg_pred::st_is_simple),
            FID_ST_IS_CLOSED => g_to_bool!(args, "st_isclosed", pg_pred::st_is_closed),
            FID_ST_IS_RING => g_to_bool!(args, "st_isring", pg_pred::st_is_ring),
            FID_ST_NUM_POINTS => g_to_int!(args, "st_numpoints", pg_acc::st_num_points),
            FID_ST_NUM_GEOMETRIES => g_to_int!(args, "st_numgeometries", pg_acc::st_num_geometries),
            FID_ST_NUM_INTERIOR_RINGS => g_to_int_result!(args, "st_numinteriorrings", pg_acc::st_num_interior_rings),
            FID_ST_NPOINTS => g_to_int!(args, "st_npoints", pg_acc::st_npoints),
            FID_ST_EXTERIOR_RING => g_to_geom!(args, "st_exteriorring", pg_acc::st_exterior_ring),
            FID_ST_INTERIOR_RING_N => {
                let g = from_wkb(arg_blob(&args, 0, "st_interiorringn")?, "st_interiorringn")?;
                let n = arg_i64(&args, 1, "st_interiorringn")? as u32;
                let r = pg_acc::st_interior_ring_n(&g, n)
                    .map_err(|e| format!("st_interiorringn: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_POINT_N => {
                let g = from_wkb(arg_blob(&args, 0, "st_pointn")?, "st_pointn")?;
                let n = arg_i64(&args, 1, "st_pointn")? as u32;
                let r = pg_acc::st_point_n(&g, n)
                    .map_err(|e| format!("st_pointn: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOMETRY_N => {
                let g = from_wkb(arg_blob(&args, 0, "st_geometryn")?, "st_geometryn")?;
                let n = arg_i64(&args, 1, "st_geometryn")? as u32;
                let r = pg_acc::st_geometry_n(&g, n)
                    .map_err(|e| format!("st_geometryn: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_START_POINT => g_to_geom!(args, "st_startpoint", pg_acc::st_start_point),
            FID_ST_END_POINT => g_to_geom!(args, "st_endpoint", pg_acc::st_end_point),
            FID_ST_BOUNDARY => g_to_geom!(args, "st_boundary", pg_proc::st_boundary),
            FID_ST_ENVELOPE => g_to_geom!(args, "st_envelope", pg_acc::st_envelope),
            FID_ST_SET_SRID => {
                let g = from_wkb(arg_blob(&args, 0, "st_setsrid")?, "st_setsrid")?;
                let srid = arg_i64(&args, 1, "st_setsrid")? as i32;
                Ok(SqlValue::Blob(g.set_srid(srid).as_wkb()))
            }

            // ── Measurements ──
            FID_ST_AREA => g_to_f64!(args, "st_area", pg_meas::st_area),
            FID_ST_LENGTH => g_to_f64!(args, "st_length", pg_meas::st_length),
            FID_ST_PERIMETER => g_to_f64!(args, "st_perimeter", pg_meas::st_perimeter),
            FID_ST_LENGTH_TWOD => g_to_f64!(args, "st_length2d", pg_meas::st_length_twod),
            FID_ST_LENGTH_THREED => g_to_f64!(args, "st_length3d", pg_meas::st_length_threed),
            FID_ST_PERIMETER_THREED => g_to_f64!(args, "st_perimeter3d", pg_meas::st_perimeter_threed),
            FID_ST_DISTANCE => gg_to_f64!(args, "st_distance", pg_meas::st_distance),
            FID_ST_DISTANCE_THREED => gg_to_f64!(args, "st_distance3d", pg_meas::st_distance_threed),
            FID_ST_MAX_DISTANCE => gg_to_f64!(args, "st_maxdistance", pg_meas::st_max_distance),
            FID_ST_MAX_DISTANCE_THREED => gg_to_f64!(args, "st_maxdistance3d", pg_meas::st_max_distance_threed),
            FID_ST_HAUSDORFF_DISTANCE => gg_to_f64!(args, "st_hausdorffdistance", pg_meas::st_hausdorff_distance),
            FID_ST_FRECHET_DISTANCE => gg_to_f64!(args, "st_frechetdistance", pg_meas::st_frechet_distance),

            // ── Predicates ──
            FID_ST_INTERSECTS => gg_to_bool!(args, "st_intersects", pg_pred::st_intersects),
            FID_ST_CONTAINS => gg_to_bool!(args, "st_contains", pg_pred::st_contains),
            FID_ST_WITHIN => gg_to_bool!(args, "st_within", pg_pred::st_within),
            FID_ST_EQUALS => gg_to_bool!(args, "st_equals", pg_pred::st_equals),
            FID_ST_DISJOINT => gg_to_bool!(args, "st_disjoint", pg_pred::st_disjoint),
            FID_ST_OVERLAPS => gg_to_bool!(args, "st_overlaps", pg_pred::st_overlaps),
            FID_ST_TOUCHES => gg_to_bool!(args, "st_touches", pg_pred::st_touches),
            FID_ST_CROSSES => gg_to_bool!(args, "st_crosses", pg_pred::st_crosses),
            FID_ST_COVERED_BY => gg_to_bool!(args, "st_coveredby", pg_pred::st_covered_by),
            FID_ST_COVERS => gg_to_bool!(args, "st_covers", pg_pred::st_covers),
            FID_ST_CONTAINS_PROPERLY => gg_to_bool!(args, "st_containsproperly", pg_pred::st_contains_properly),
            FID_ST_3D_INTERSECTS => gg_to_bool!(args, "st_3dintersects", pg_pred::st_intersects_threed),
            // st-3d-disjoint isn't exported by postgis-wasm; alias to st-disjoint.
            FID_ST_3D_DISJOINT => gg_to_bool!(args, "st_3ddisjoint", pg_pred::st_disjoint),

            // ── Processing ──
            FID_ST_BUFFER => gd_to_geom!(args, "st_buffer", pg_proc::st_buffer),
            FID_ST_INTERSECTION => gg_to_geom!(args, "st_intersection", pg_proc::st_intersection),
            FID_ST_UNION => gg_to_geom!(args, "st_union", pg_proc::st_union),
            FID_ST_DIFFERENCE => gg_to_geom!(args, "st_difference", pg_proc::st_difference),
            FID_ST_SYM_DIFFERENCE => gg_to_geom!(args, "st_symdifference", pg_proc::st_sym_difference),
            FID_ST_UNARY_UNION => g_to_geom!(args, "st_unaryunion", pg_proc::st_unary_union),
            FID_ST_SIMPLIFY => gd_to_geom!(args, "st_simplify", pg_proc::st_simplify),
            FID_ST_SIMPLIFY_PT => gd_to_geom!(args, "st_simplifypreservetopology", pg_proc::st_simplify_preserve_topology),
            FID_ST_SIMPLIFY_VW => gd_to_geom!(args, "st_simplifyvw", pg_proc::st_simplify_vw),
            FID_ST_CONVEX_HULL => g_to_geom!(args, "st_convexhull", pg_proc::st_convex_hull),
            FID_ST_CONCAVE_HULL => gd_to_geom!(args, "st_concavehull", pg_proc::st_concave_hull),
            FID_ST_CENTROID => g_to_geom!(args, "st_centroid", pg_proc::st_centroid),
            FID_ST_POINT_ON_SURFACE => g_to_geom!(args, "st_pointonsurface", pg_proc::st_point_on_surface),
            FID_ST_ORIENTED_ENVELOPE => g_to_geom!(args, "st_orientedenvelope", pg_proc::st_oriented_envelope),
            FID_ST_MIN_BOUNDING_CIRCLE => g_to_geom!(args, "st_minimumboundingcircle", pg_proc::st_minimum_bounding_circle),
            FID_ST_LINE_MERGE => g_to_geom!(args, "st_linemerge", pg_proc::st_line_merge),
            FID_ST_MAKE_VALID => g_to_geom!(args, "st_makevalid", pg_proc::st_make_valid),
            FID_ST_REVERSE => g_to_geom!(args, "st_reverse", pg_proc::st_reverse),
            FID_ST_FLIP_COORDINATES => g_to_geom!(args, "st_flipcoordinates", pg_xform::st_flip_coordinates),
            FID_ST_FORCE_2D => g_to_geom_inf!(args, "st_force2d", pg_xform::st_force_twod),
            FID_ST_FORCE_3D => g_to_geom_inf!(args, "st_force3d", pg_xform::st_force_threed),
            FID_ST_MULTI => g_to_geom!(args, "st_multi", pg_acc::st_multi),
            FID_ST_COLLECTION_HOMOGENIZE => g_to_geom!(args, "st_collectionhomogenize", pg_acc::st_collection_homogenize),

            // ── Output ──
            FID_ST_ASTEXT => g_to_string!(args, "st_astext", pg_out::st_as_text),
            FID_ST_ASBINARY => g_to_blob!(args, "st_asbinary", pg_out::st_as_binary),
            FID_ST_AS_EWKT => g_to_string!(args, "st_asewkt", pg_out::st_as_ewkt),
            FID_ST_AS_EWKB => g_to_blob!(args, "st_asewkb", pg_out::st_as_ewkb),
            FID_ST_AS_HEXEWKB => g_to_string!(args, "st_ashexewkb", pg_out::st_as_hexewkb),
            FID_ST_AS_GEOJSON => g_to_string!(args, "st_asgeojson", pg_out::st_as_geojson),
            FID_ST_AS_SVG => g_to_string_result!(args, "st_assvg", pg_out::st_as_svg),
            FID_ST_AS_KML => g_to_string_result!(args, "st_askml", pg_out::st_as_kml),
            FID_ST_AS_GML => {
                let g = from_wkb(arg_blob(&args, 0, "st_asgml")?, "st_asgml")?;
                let s = pg_out::st_as_gml(&g, None, None)
                    .map_err(|e| format!("st_asgml: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Text(s))
            }
            FID_ST_AS_X3D => g_to_string_result!(args, "st_asx3d", pg_out::st_as_x3d),
            FID_ST_SUMMARY => g_to_string!(args, "st_summary", pg_out::st_summary),
            FID_ST_GEOHASH => {
                let g = from_wkb(arg_blob(&args, 0, "st_geohash")?, "st_geohash")?;
                let s = pg_out::st_geohash(&g, None)
                    .map_err(|e| format!("st_geohash: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Text(s))
            }

            other => Err(format!("postgis bridge: unknown func id {other}")),
        }
    }
}

bindings::export!(PostgisBridge with_types_in bindings);
