//! Google S2 geometry library, exposed via SQLite scalars.
//!
//! Per PLAN-extensions-and-handlers.md #9. The `s2` crate
//! (yjh0502/rust-s2) is the most-maintained pure-rust port — its
//! Cell/CellID/LatLng/RegionCoverer surfaces are stable; loop +
//! polygon are pending upstream, so v1's covering accepts a rect
//! (or a list of points whose bounding box defines a rect)
//! instead of an arbitrary polygon. Documented in the Cargo.toml
//! description.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use s2::cellid::{CellID, MAX_LEVEL};
use s2::latlng::LatLng;
use s2::rect::Rect;
use s2::region::RegionCoverer;

// ── Cell <-> i64 bridge ─────────────────────────────────────────
//
// S2 cell ids fit in u64. SQLite INTEGER is i64. S2's face takes
// the top 3 bits [61..63], so faces 4 and 5 set the sign bit when
// the cell is reinterpreted as i64 — meaning some valid cells
// surface as negative integers in SQL. The bitcast is lossless
// either way (round-trips through h2_token_to_cell or a direct
// i64 store), it just isn't sign-monotonic. If a caller needs
// an unsigned-looking representation, hop through the hex token.

fn cell_from_i64(v: i64) -> CellID {
    CellID(v as u64)
}

fn cell_to_i64(c: CellID) -> i64 {
    c.0 as i64
}

fn parse_level(l: i64) -> Result<u64, String> {
    if !(0..=(MAX_LEVEL as i64)).contains(&l) {
        return Err(format!("s2: level {l} out of range 0..={}", MAX_LEVEL));
    }
    Ok(l as u64)
}

// ── Algorithm core (Guest-shape-free) ───────────────────────────
//
// Pulled out of the WIT export so unit tests + a future embed path
// can reach them without going through the wit-bindgen-generated
// Guest impl.

pub fn s2_latlng_to_cell(lat: f64, lng: f64, level: i64) -> Result<i64, String> {
    let lvl = parse_level(level)?;
    let ll = LatLng::from_degrees(lat, lng);
    if !ll.is_valid() {
        return Err(format!("s2: invalid lat/lng ({lat}, {lng})"));
    }
    // CellID::from(&LatLng) produces a leaf cell (level 30); walk
    // up to the requested level.
    let leaf: CellID = (&ll).into();
    Ok(cell_to_i64(leaf.parent(lvl)))
}

pub fn s2_cell_to_latlng(cell: i64) -> Result<String, String> {
    let c = cell_from_i64(cell);
    if !c.is_valid() {
        return Err(format!("s2: invalid cell {cell:#x}"));
    }
    let ll: LatLng = (&c).into();
    Ok(format!("{},{}", ll.lat.deg(), ll.lng.deg()))
}

pub fn s2_cell_to_token(cell: i64) -> Result<String, String> {
    let c = cell_from_i64(cell);
    Ok(c.to_token())
}

pub fn s2_token_to_cell(token: &str) -> Result<i64, String> {
    let c = CellID::from_token(token);
    // `from_token` returns CellID(0) on a parse error — the s2
    // crate doesn't distinguish "bad input" from "the sentinel
    // zero." Surface bad parses to callers instead of silently
    // returning 0; a valid encoded cell can never be 0 because
    // the face bits would be set.
    if c.0 == 0 {
        return Err(format!("s2: invalid cell token {token:?}"));
    }
    Ok(cell_to_i64(c))
}

pub fn s2_cell_level(cell: i64) -> Result<i64, String> {
    let c = cell_from_i64(cell);
    if !c.is_valid() {
        return Err(format!("s2: invalid cell {cell:#x}"));
    }
    Ok(c.level() as i64)
}

pub fn s2_cell_parent(cell: i64, level: i64) -> Result<i64, String> {
    let c = cell_from_i64(cell);
    if !c.is_valid() {
        return Err(format!("s2: invalid cell {cell:#x}"));
    }
    let lvl = parse_level(level)?;
    if lvl > c.level() {
        return Err(format!(
            "s2: cell at level {} has no parent at level {}",
            c.level(),
            lvl
        ));
    }
    Ok(cell_to_i64(c.parent(lvl)))
}

pub fn s2_cell_children(cell: i64) -> Result<String, String> {
    let c = cell_from_i64(cell);
    if !c.is_valid() {
        return Err(format!("s2: invalid cell {cell:#x}"));
    }
    if c.is_leaf() {
        return Err(format!(
            "s2: cell {cell:#x} is a leaf (level {}), no children",
            c.level()
        ));
    }
    let kids = c.children();
    let arr: Vec<serde_json::Value> = kids
        .iter()
        .map(|child| serde_json::json!(cell_to_i64(*child)))
        .collect();
    Ok(serde_json::Value::Array(arr).to_string())
}

pub fn s2_cell_is_valid(cell: i64) -> bool {
    cell_from_i64(cell).is_valid()
}

/// Parse the covering input JSON into a `Rect`.
///
/// Accepted shapes (both lat/lng in degrees):
///   * `{"lat_lo": <f>, "lng_lo": <f>, "lat_hi": <f>, "lng_hi": <f>}` —
///     explicit rect.
///   * `[{"lat":..,"lng":..}, ..]` — list of points; the bounding
///     box of the points is used.
///   * `[[lat, lng], ..]` — same as above, pair form.
///
/// Polygon-shaped input (`{"type":"Polygon", ...}`) is not yet
/// supported because the s2 crate's loop/polygon types are
/// upstream-pending; bounding-box semantics is documented and
/// surfaces a recognizable error for unknown shapes.
fn parse_rect(json: &str) -> Result<Rect, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("s2_covering: bad JSON: {e}"))?;

    if let Some(obj) = v.as_object() {
        let needed = ["lat_lo", "lng_lo", "lat_hi", "lng_hi"];
        if needed.iter().all(|k| obj.contains_key(*k)) {
            let get = |k: &str| -> Result<f64, String> {
                obj[k]
                    .as_f64()
                    .ok_or_else(|| format!("s2_covering: {k} must be a number"))
            };
            let lat_lo = get("lat_lo")?;
            let lng_lo = get("lng_lo")?;
            let lat_hi = get("lat_hi")?;
            let lng_hi = get("lng_hi")?;
            return Ok(Rect::from_degrees(lat_lo, lng_lo, lat_hi, lng_hi));
        }
    }

    if let Some(arr) = v.as_array() {
        if arr.is_empty() {
            return Err("s2_covering: empty point list".into());
        }
        let mut points: Vec<LatLng> = Vec::with_capacity(arr.len());
        for (i, item) in arr.iter().enumerate() {
            if let Some(obj) = item.as_object() {
                let lat = obj
                    .get("lat")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| format!("s2_covering: point[{i}].lat missing/non-numeric"))?;
                let lng = obj
                    .get("lng")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| format!("s2_covering: point[{i}].lng missing/non-numeric"))?;
                points.push(LatLng::from_degrees(lat, lng));
            } else if let Some(pair) = item.as_array() {
                if pair.len() != 2 {
                    return Err(format!(
                        "s2_covering: point[{i}] pair must have 2 elements"
                    ));
                }
                let lat = pair[0]
                    .as_f64()
                    .ok_or_else(|| format!("s2_covering: point[{i}][0] non-numeric"))?;
                let lng = pair[1]
                    .as_f64()
                    .ok_or_else(|| format!("s2_covering: point[{i}][1] non-numeric"))?;
                points.push(LatLng::from_degrees(lat, lng));
            } else {
                return Err(format!("s2_covering: point[{i}] must be obj or pair"));
            }
        }
        // Fold the points into a bounding rect.
        let mut rect = Rect::from_point_pair(&points[0], &points[0]);
        for p in points.iter().skip(1) {
            rect = rect.union(&Rect::from_point_pair(p, p));
        }
        return Ok(rect);
    }

    Err("s2_covering: input must be a rect object or a point list".into())
}

pub fn s2_covering(json_rect: &str, max_cells: i64) -> Result<String, String> {
    if max_cells <= 0 {
        return Err(format!("s2_covering: max_cells must be > 0, got {max_cells}"));
    }
    let rect = parse_rect(json_rect)?;
    let coverer = RegionCoverer {
        min_level: 0,
        max_level: MAX_LEVEL as u8,
        level_mod: 1,
        max_cells: max_cells as usize,
    };
    let union = coverer.covering(&rect);
    let arr: Vec<serde_json::Value> = union
        .0
        .iter()
        .map(|c| serde_json::json!(cell_to_i64(*c)))
        .collect();
    Ok(serde_json::Value::Array(arr).to_string())
}

// ── WIT export ──────────────────────────────────────────────────

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

    const FID_LATLNG_TO_CELL: u64 = 1;
    const FID_CELL_TO_LATLNG: u64 = 2;
    const FID_CELL_TO_TOKEN: u64 = 3;
    const FID_TOKEN_TO_CELL: u64 = 4;
    const FID_CELL_LEVEL: u64 = 5;
    const FID_CELL_PARENT: u64 = 6;
    const FID_CELL_CHILDREN: u64 = 7;
    const FID_CELL_IS_VALID: u64 = 8;
    const FID_COVERING: u64 = 9;

    struct Ext;

    fn arg_real(args: &[SqlValue], i: usize, fname: &str) -> Result<f64, String> {
        match args.get(i) {
            Some(SqlValue::Real(r)) => Ok(*r),
            Some(SqlValue::Integer(n)) => Ok(*n as f64),
            _ => Err(format!("{fname}: numeric arg at {i}")),
        }
    }
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(r)) => Ok(*r as i64),
            _ => Err(format!("{fname}: integer arg at {i}")),
        }
    }
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "s2".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_LATLNG_TO_CELL, "s2_latlng_to_cell", 3),
                    s(FID_CELL_TO_LATLNG, "s2_cell_to_latlng", 1),
                    s(FID_CELL_TO_TOKEN, "s2_cell_to_token", 1),
                    s(FID_TOKEN_TO_CELL, "s2_token_to_cell", 1),
                    s(FID_CELL_LEVEL, "s2_cell_level", 1),
                    s(FID_CELL_PARENT, "s2_cell_parent", 2),
                    s(FID_CELL_CHILDREN, "s2_cell_children", 1),
                    s(FID_CELL_IS_VALID, "s2_cell_is_valid", 1),
                    s(FID_COVERING, "s2_covering", 2),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_LATLNG_TO_CELL => {
                    let lat = arg_real(&args, 0, "s2_latlng_to_cell")?;
                    let lng = arg_real(&args, 1, "s2_latlng_to_cell")?;
                    let level = arg_int(&args, 2, "s2_latlng_to_cell")?;
                    super::s2_latlng_to_cell(lat, lng, level).map(SqlValue::Integer)
                }
                FID_CELL_TO_LATLNG => super::s2_cell_to_latlng(arg_int(&args, 0, "s2_cell_to_latlng")?)
                    .map(SqlValue::Text),
                FID_CELL_TO_TOKEN => super::s2_cell_to_token(arg_int(&args, 0, "s2_cell_to_token")?)
                    .map(SqlValue::Text),
                FID_TOKEN_TO_CELL => {
                    let t = arg_text(&args, 0, "s2_token_to_cell")?;
                    super::s2_token_to_cell(&t).map(SqlValue::Integer)
                }
                FID_CELL_LEVEL => super::s2_cell_level(arg_int(&args, 0, "s2_cell_level")?)
                    .map(SqlValue::Integer),
                FID_CELL_PARENT => {
                    let cell = arg_int(&args, 0, "s2_cell_parent")?;
                    let level = arg_int(&args, 1, "s2_cell_parent")?;
                    super::s2_cell_parent(cell, level).map(SqlValue::Integer)
                }
                FID_CELL_CHILDREN => super::s2_cell_children(arg_int(&args, 0, "s2_cell_children")?)
                    .map(SqlValue::Text),
                FID_CELL_IS_VALID => {
                    let cell = arg_int(&args, 0, "s2_cell_is_valid")?;
                    Ok(SqlValue::Integer(super::s2_cell_is_valid(cell) as i64))
                }
                FID_COVERING => {
                    let json = arg_text(&args, 0, "s2_covering")?;
                    let max_cells = arg_int(&args, 1, "s2_covering")?;
                    super::s2_covering(&json, max_cells).map(SqlValue::Text)
                }
                other => Err(format!("s2: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SF (37.7749, -122.4194) at level 12 — round-trip lat/lng
    /// within S2's leaf-cell precision (~1 m at level 30, much
    /// looser at level 12). Acceptance criterion: parses back to
    /// approximately the same lat/lng.
    #[test]
    fn sf_level_12_roundtrip() {
        let cell = s2_latlng_to_cell(37.7749, -122.4194, 12).unwrap();
        let s = s2_cell_to_latlng(cell).unwrap();
        let parts: Vec<&str> = s.split(',').collect();
        let lat: f64 = parts[0].parse().unwrap();
        let lng: f64 = parts[1].parse().unwrap();
        // Level 12 cells are ~5 km on a side; the cell's centre is
        // within ~0.05° of the input.
        assert!((lat - 37.7749).abs() < 0.05, "lat drift {}", lat - 37.7749);
        assert!((lng - (-122.4194)).abs() < 0.05, "lng drift {}", lng - (-122.4194));
    }

    #[test]
    fn children_count_is_four() {
        let cell = s2_latlng_to_cell(37.7749, -122.4194, 12).unwrap();
        let s = s2_cell_children(cell).unwrap();
        let arr: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 4);
    }

    #[test]
    fn covering_bounds_max_cells() {
        // ~1 km box around SF.
        let json = r#"{"lat_lo":37.77,"lng_lo":-122.43,"lat_hi":37.78,"lng_hi":-122.41}"#;
        let s = s2_covering(json, 8).unwrap();
        let arr: serde_json::Value = serde_json::from_str(&s).unwrap();
        let n = arr.as_array().unwrap().len();
        assert!(n > 0 && n <= 8, "covering len {n} not in (0, 8]");
    }

    #[test]
    fn token_roundtrip() {
        let cell = s2_latlng_to_cell(37.7749, -122.4194, 12).unwrap();
        let tok = s2_cell_to_token(cell).unwrap();
        let back = s2_token_to_cell(&tok).unwrap();
        assert_eq!(cell, back);
    }

    #[test]
    fn level_matches_input() {
        let cell = s2_latlng_to_cell(37.7749, -122.4194, 12).unwrap();
        assert_eq!(s2_cell_level(cell).unwrap(), 12);
    }
}
