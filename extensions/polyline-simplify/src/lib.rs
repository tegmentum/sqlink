//! Polyline simplification: Douglas-Peucker + Visvalingam-Whyatt.
//!
//! Wraps `geo` 0.30's `Simplify` / `SimplifyVw` traits behind a JSON
//! SQL surface so callers can simplify routes without leaving SQL.
//!
//! Function surface:
//!   polyline_simplify_dp(points_json, tolerance)  -> JSON
//!   polyline_simplify_vw(points_json, tolerance)  -> JSON
//!   polyline_simplify_version()                   -> TEXT
//!
//! `points_json` is a JSON array of `[lng, lat]` pairs (matches
//! the `google-polyline` decode shape). Output preserves first +
//! last point; intermediate points within `tolerance` of the
//! simplified line are dropped.
//!
//! Bad JSON / non-array input -> NULL (not an error) so the surface
//! composes inside CASE / WHERE without explicit error handling.

extern crate alloc;

// Re-exported so the test module can exercise the pure-Rust
// helpers without dragging in wit-bindgen.
pub fn simplify_dp(points_json: &str, tolerance: f64) -> Option<alloc::string::String> {
    let pts = parse_points(points_json)?;
    if pts.len() <= 2 {
        return Some(emit_points(&pts));
    }
    use geo::algorithm::Simplify;
    use geo::LineString;
    let ls: LineString<f64> = pts.iter().copied().map(|(x, y)| (x, y)).collect();
    let simplified = ls.simplify(&tolerance);
    let out: alloc::vec::Vec<(f64, f64)> =
        simplified.0.iter().map(|c| (c.x, c.y)).collect();
    Some(emit_points(&out))
}

pub fn simplify_vw(points_json: &str, tolerance: f64) -> Option<alloc::string::String> {
    let pts = parse_points(points_json)?;
    if pts.len() <= 2 {
        return Some(emit_points(&pts));
    }
    use geo::algorithm::SimplifyVw;
    use geo::LineString;
    let ls: LineString<f64> = pts.iter().copied().map(|(x, y)| (x, y)).collect();
    let simplified = ls.simplify_vw(&tolerance);
    let out: alloc::vec::Vec<(f64, f64)> =
        simplified.0.iter().map(|c| (c.x, c.y)).collect();
    Some(emit_points(&out))
}

pub fn version() -> alloc::string::String {
    alloc::string::ToString::to_string(env!("CARGO_PKG_VERSION"))
}

/// Parse a JSON array of `[lng, lat]` pairs into `Vec<(f64, f64)>`.
/// Returns None on any structural problem -- bad JSON, non-array
/// root, non-array element, wrong arity, or non-numeric coord.
fn parse_points(json: &str) -> Option<alloc::vec::Vec<(f64, f64)>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let arr = v.as_array()?;
    let mut out = alloc::vec::Vec::with_capacity(arr.len());
    for pt in arr {
        let pair = pt.as_array()?;
        if pair.len() != 2 {
            return None;
        }
        let x = pair[0].as_f64()?;
        let y = pair[1].as_f64()?;
        out.push((x, y));
    }
    Some(out)
}

/// Serialize `[(lng, lat), ...]` as a compact JSON array of arrays.
/// Hand-rolled so the output stays a plain `[[x,y],...]` shape that
/// round-trips through `json()` cleanly -- `serde_json::Value` would
/// promote 1.0 to "1.0" but `5.0` shows as `5` via the `as_f64` path,
/// which is fine for our purposes.
fn emit_points(pts: &[(f64, f64)]) -> alloc::string::String {
    use alloc::string::String;
    use core::fmt::Write;
    let mut s = String::with_capacity(pts.len() * 24 + 2);
    s.push('[');
    for (i, (x, y)) in pts.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        // f64 -> serde_json::Number -> string keeps the canonical
        // shortest round-trip representation.
        let xn = serde_json::Number::from_f64(*x)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "null".into());
        let yn = serde_json::Number::from_f64(*y)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "null".into());
        let _ = write!(s, "[{xn},{yn}]");
    }
    s.push(']');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dp_drops_collinear_points() {
        // Straight line: middle points should collapse.
        let input = "[[0,0],[1,1],[2,2],[3,3],[4,4]]";
        let out = simplify_dp(input, 0.001).unwrap();
        assert_eq!(out, "[[0.0,0.0],[4.0,4.0]]");
    }

    #[test]
    fn dp_preserves_corners() {
        // Sharp corner at (5,4) -- should survive a small tolerance.
        let input = "[[0,0],[5,4],[11,5.5],[17.3,3.2],[27.8,0.1]]";
        let out = simplify_dp(input, 1.0).unwrap();
        // First + last preserved; the corner at (5,4) survives.
        assert!(out.starts_with("[[0.0,0.0]"));
        assert!(out.ends_with("[27.8,0.1]]"));
        assert!(out.contains("[5.0,4.0]"));
    }

    #[test]
    fn vw_two_points_passthrough() {
        let out = simplify_vw("[[0,0],[1,1]]", 1.0).unwrap();
        assert_eq!(out, "[[0.0,0.0],[1.0,1.0]]");
    }

    #[test]
    fn bad_json_is_none() {
        assert!(simplify_dp("not json", 1.0).is_none());
        assert!(simplify_dp("{\"x\":1}", 1.0).is_none()); // object not array
        assert!(simplify_dp("[1,2,3]", 1.0).is_none()); // not pairs
        assert!(simplify_dp("[[1]]", 1.0).is_none()); // wrong arity
        assert!(simplify_dp("[[\"a\",\"b\"]]", 1.0).is_none()); // not numeric
    }

    #[test]
    fn empty_array_round_trips() {
        let out = simplify_dp("[]", 1.0).unwrap();
        assert_eq!(out, "[]");
    }

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }
}

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

    const FID_DP: u64 = 1;
    const FID_VW: u64 = 2;
    const FID_VERSION: u64 = 3;

    struct Ext;

    /// Optional-text helper: NULL passes through as None; TEXT /
    /// BLOB surface as the underlying string. INTEGER / REAL
    /// reject (no sensible coercion for a JSON document).
    fn opt_text(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            None => Err(format!("{fname}: missing arg at {i}")),
            Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(SqlValue::Blob(b)) => match core::str::from_utf8(b) {
                Ok(s) => Ok(Some(s.to_string())),
                Err(_) => Ok(None), // non-utf8 blob -> NULL not error
            },
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    /// Optional-real helper: NULL -> None; INTEGER coerces to f64;
    /// TEXT parses as f64 (so `'0.5'` works the same as `0.5`).
    fn opt_real(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<f64>, String> {
        match args.get(i) {
            None => Err(format!("{fname}: missing arg at {i}")),
            Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Real(r)) => Ok(Some(*r)),
            Some(SqlValue::Integer(n)) => Ok(Some(*n as f64)),
            Some(SqlValue::Text(s)) => match s.parse::<f64>() {
                Ok(r) => Ok(Some(r)),
                Err(_) => Ok(None),
            },
            _ => Err(format!("{fname}: numeric arg at {i}")),
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
                name: "polyline_simplify".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_DP, "polyline_simplify_dp", 2),
                    s(FID_VW, "polyline_simplify_vw", 2),
                    s(FID_VERSION, "polyline_simplify_version", 0),
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
                FID_DP => {
                    let json = match opt_text(&args, 0, "polyline_simplify_dp")? {
                        None => return Ok(SqlValue::Null),
                        Some(s) => s,
                    };
                    let tol = match opt_real(&args, 1, "polyline_simplify_dp")? {
                        None => return Ok(SqlValue::Null),
                        Some(r) => r,
                    };
                    Ok(match super::simplify_dp(&json, tol) {
                        Some(out) => SqlValue::Text(out),
                        None => SqlValue::Null,
                    })
                }
                FID_VW => {
                    let json = match opt_text(&args, 0, "polyline_simplify_vw")? {
                        None => return Ok(SqlValue::Null),
                        Some(s) => s,
                    };
                    let tol = match opt_real(&args, 1, "polyline_simplify_vw")? {
                        None => return Ok(SqlValue::Null),
                        Some(r) => r,
                    };
                    Ok(match super::simplify_vw(&json, tol) {
                        Some(out) => SqlValue::Text(out),
                        None => SqlValue::Null,
                    })
                }
                FID_VERSION => Ok(SqlValue::Text(super::version())),
                other => Err(format!("polyline_simplify: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
