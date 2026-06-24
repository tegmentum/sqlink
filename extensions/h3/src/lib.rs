//! H3 hexagonal hierarchical geospatial index (Uber H3 / h3o port).
//!
//! Exposes the H3 cell API with INTEGER cell indices, following
//! PLAN-extensions-and-handlers.md #8. The existing `geo`
//! extension exposes a smaller H3 surface keyed on TEXT cell hex;
//! this extension's contract is the i64 cell representation
//! ("H3Index as i64") that maps onto SQLite primary keys and joins
//! without TEXT round-tripping.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use h3o::{CellIndex, LatLng, Resolution};

// ── Cell <-> i64 bridge ─────────────────────────────────────────
//
// H3 cell indices fit in u64. SQLite INTEGER is i64. The H3 spec
// reserves the top 4 bits for the mode (mode=1 for cells), so the
// high bit is always 0 → a u64 cell value is always positive when
// reinterpreted as i64 and round-trips losslessly through bitcast.

/// Bitcast a SQLite i64 cell into a `CellIndex`. Validates the
/// bit pattern through h3o's `TryFrom<u64>` so junk inputs surface
/// as an error rather than going on to produce wrong-looking
/// geometry downstream.
fn cell_from_i64(v: i64) -> Result<CellIndex, String> {
    CellIndex::try_from(v as u64).map_err(|e| format!("h3: invalid cell {v:#x}: {e}"))
}

fn cell_to_i64(c: CellIndex) -> i64 {
    u64::from(c) as i64
}

fn parse_resolution(r: i64) -> Result<Resolution, String> {
    if !(0..=15).contains(&r) {
        return Err(format!("h3: resolution {r} out of range 0..=15"));
    }
    Resolution::try_from(r as u8).map_err(|e| format!("h3: bad resolution {r}: {e}"))
}

// ── Algorithm core (Guest-shape-free) ───────────────────────────
//
// Pulled out of the WIT export so unit tests + future embed path
// can reach them without going through the wit-bindgen-generated
// Guest impl.

pub fn h3_latlng_to_cell(lat: f64, lng: f64, res: i64) -> Result<i64, String> {
    let resolution = parse_resolution(res)?;
    let latlng = LatLng::new(lat, lng).map_err(|e| format!("h3: bad coords: {e}"))?;
    Ok(cell_to_i64(latlng.to_cell(resolution)))
}

pub fn h3_cell_to_latlng(cell: i64) -> Result<String, String> {
    let c = cell_from_i64(cell)?;
    let ll = LatLng::from(c);
    Ok(format!("{},{}", ll.lat(), ll.lng()))
}

pub fn h3_cell_to_boundary(cell: i64) -> Result<String, String> {
    let c = cell_from_i64(cell)?;
    let boundary = c.boundary();
    let verts: Vec<serde_json::Value> = boundary
        .iter()
        .map(|ll| serde_json::Value::String(format!("{},{}", ll.lat(), ll.lng())))
        .collect();
    Ok(serde_json::Value::Array(verts).to_string())
}

pub fn h3_cell_resolution(cell: i64) -> Result<i64, String> {
    Ok(cell_from_i64(cell)?.resolution() as i64)
}

pub fn h3_cell_parent(cell: i64, res: i64) -> Result<i64, String> {
    let c = cell_from_i64(cell)?;
    let r = parse_resolution(res)?;
    c.parent(r)
        .map(cell_to_i64)
        .ok_or_else(|| format!("h3: cell at res {} has no parent at res {}", c.resolution() as u8, res))
}

pub fn h3_cell_children(cell: i64, res: i64) -> Result<String, String> {
    let c = cell_from_i64(cell)?;
    let r = parse_resolution(res)?;
    let children: Vec<serde_json::Value> = c
        .children(r)
        .map(|child| serde_json::json!(cell_to_i64(child)))
        .collect();
    Ok(serde_json::Value::Array(children).to_string())
}

pub fn h3_neighbors(cell: i64) -> Result<String, String> {
    let c = cell_from_i64(cell)?;
    // grid_disk with k=1 returns the cell + its ring-1 neighbors;
    // filter the centre out so a non-pentagon cell yields exactly
    // 6 cells (5 for a pentagon).
    let neighbors: Vec<serde_json::Value> = c
        .grid_disk::<Vec<_>>(1)
        .into_iter()
        .filter(|n| *n != c)
        .map(|n| serde_json::json!(cell_to_i64(n)))
        .collect();
    Ok(serde_json::Value::Array(neighbors).to_string())
}

pub fn h3_k_ring(cell: i64, k: i64) -> Result<String, String> {
    if k < 0 {
        return Err(format!("h3: k must be >= 0, got {k}"));
    }
    let c = cell_from_i64(cell)?;
    let cells: Vec<serde_json::Value> = c
        .grid_disk::<Vec<_>>(k as u32)
        .into_iter()
        .map(|n| serde_json::json!(cell_to_i64(n)))
        .collect();
    Ok(serde_json::Value::Array(cells).to_string())
}

/// Returns `Ok(None)` when the two cells aren't grid-distance
/// comparable (different resolutions, or too far apart for h3o's
/// local-IJ math). Surfaced as SQL NULL.
pub fn h3_distance(a: i64, b: i64) -> Result<Option<i64>, String> {
    let ca = cell_from_i64(a)?;
    let cb = cell_from_i64(b)?;
    Ok(ca.grid_distance(cb).ok().map(|d| d as i64))
}

pub fn h3_is_valid(cell: i64) -> bool {
    CellIndex::try_from(cell as u64).is_ok()
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
    const FID_CELL_TO_BOUNDARY: u64 = 3;
    const FID_CELL_RESOLUTION: u64 = 4;
    const FID_CELL_PARENT: u64 = 5;
    const FID_CELL_CHILDREN: u64 = 6;
    const FID_NEIGHBORS: u64 = 7;
    const FID_K_RING: u64 = 8;
    const FID_DISTANCE: u64 = 9;
    const FID_IS_VALID: u64 = 10;

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
                name: "h3".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_LATLNG_TO_CELL, "h3_latlng_to_cell", 3),
                    s(FID_CELL_TO_LATLNG, "h3_cell_to_latlng", 1),
                    s(FID_CELL_TO_BOUNDARY, "h3_cell_to_boundary", 1),
                    s(FID_CELL_RESOLUTION, "h3_cell_resolution", 1),
                    s(FID_CELL_PARENT, "h3_cell_parent", 2),
                    s(FID_CELL_CHILDREN, "h3_cell_children", 2),
                    s(FID_NEIGHBORS, "h3_neighbors", 1),
                    s(FID_K_RING, "h3_k_ring", 2),
                    s(FID_DISTANCE, "h3_distance", 2),
                    s(FID_IS_VALID, "h3_is_valid", 1),
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
                optional_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_LATLNG_TO_CELL => {
                    let lat = arg_real(&args, 0, "h3_latlng_to_cell")?;
                    let lng = arg_real(&args, 1, "h3_latlng_to_cell")?;
                    let res = arg_int(&args, 2, "h3_latlng_to_cell")?;
                    super::h3_latlng_to_cell(lat, lng, res).map(SqlValue::Integer)
                }
                FID_CELL_TO_LATLNG => super::h3_cell_to_latlng(arg_int(&args, 0, "h3_cell_to_latlng")?)
                    .map(SqlValue::Text),
                FID_CELL_TO_BOUNDARY => {
                    super::h3_cell_to_boundary(arg_int(&args, 0, "h3_cell_to_boundary")?)
                        .map(SqlValue::Text)
                }
                FID_CELL_RESOLUTION => {
                    super::h3_cell_resolution(arg_int(&args, 0, "h3_cell_resolution")?)
                        .map(SqlValue::Integer)
                }
                FID_CELL_PARENT => {
                    let cell = arg_int(&args, 0, "h3_cell_parent")?;
                    let res = arg_int(&args, 1, "h3_cell_parent")?;
                    super::h3_cell_parent(cell, res).map(SqlValue::Integer)
                }
                FID_CELL_CHILDREN => {
                    let cell = arg_int(&args, 0, "h3_cell_children")?;
                    let res = arg_int(&args, 1, "h3_cell_children")?;
                    super::h3_cell_children(cell, res).map(SqlValue::Text)
                }
                FID_NEIGHBORS => super::h3_neighbors(arg_int(&args, 0, "h3_neighbors")?)
                    .map(SqlValue::Text),
                FID_K_RING => {
                    let cell = arg_int(&args, 0, "h3_k_ring")?;
                    let k = arg_int(&args, 1, "h3_k_ring")?;
                    super::h3_k_ring(cell, k).map(SqlValue::Text)
                }
                FID_DISTANCE => {
                    let a = arg_int(&args, 0, "h3_distance")?;
                    let b = arg_int(&args, 1, "h3_distance")?;
                    match super::h3_distance(a, b)? {
                        Some(d) => Ok(SqlValue::Integer(d)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_IS_VALID => {
                    let cell = arg_int(&args, 0, "h3_is_valid")?;
                    Ok(SqlValue::Integer(super::h3_is_valid(cell) as i64))
                }
                other => Err(format!("h3: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known H3 reference: the published H3 string `8928308280fffff`
    /// corresponds to (37.775938728915946, -122.41795063018799) at
    /// res 9 — the full-precision SF coords from the H3 docs, not
    /// the rounded 37.7749/-122.4194 which lands in an adjacent
    /// hex.
    #[test]
    fn sf_res9_known_vector() {
        let cell = h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9).unwrap();
        let expected: i64 = 0x8928308280fffff_u64 as i64;
        assert_eq!(cell, expected, "cell {:#x} != expected {:#x}", cell, expected);
    }

    #[test]
    fn distance_to_self_is_zero() {
        let cell = h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9).unwrap();
        assert_eq!(h3_distance(cell, cell).unwrap(), Some(0));
    }

    #[test]
    fn children_count_is_seven_for_hexagon() {
        let cell = h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9).unwrap();
        let children_json = h3_cell_children(cell, 10).unwrap();
        let arr: serde_json::Value = serde_json::from_str(&children_json).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 7);
    }

    #[test]
    fn boundary_has_six_verts_for_non_pentagon() {
        let cell = h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9).unwrap();
        let b = h3_cell_to_boundary(cell).unwrap();
        let arr: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 6);
    }
}
