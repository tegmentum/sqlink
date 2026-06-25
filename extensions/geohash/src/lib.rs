//! Geohash extension for SQLite.
//!
//! Geohash (Gustavo Niemeyer, 2008; public domain) is a Z-order curve
//! over (latitude, longitude) projected onto a 32-symbol base-32
//! alphabet. Longer strings = finer rectangular cells; the popular
//! length-9 default gives ~5m precision (perfect for street-level GPS).
//!
//! Function surface (per session-2026-06 batch brief):
//!
//!   geohash_encode(lat, lng, [precision]) -> text
//!     Encode a WGS84 lat/lng to a base-32 geohash string. `precision`
//!     is the character count, range 1..=12 (the geohash crate's hard
//!     cap, dictated by f64 mantissa width). Default 9 (~5m).
//!
//!   geohash_decode(s) -> text (JSON array `[lat, lng]`)
//!     Decode a geohash to its center lat/lng. Output is a 2-element
//!     JSON array so callers can pipe through `->>0` / `->>1` instead
//!     of parsing CSV.
//!
//!   geohash_neighbors(s) -> text (JSON object `{n,ne,e,se,s,sw,w,nw}`)
//!     Return the 8 neighboring cells, each as a geohash string of the
//!     same length as the input.
//!
//!   geohash_version() -> text
//!     `geohash ext <ver>; geohash <crate-version>`.
//!
//! ### Coordinate convention
//!
//! Geohash is encoded over (lat, lng) like every other ext in this
//! repo (mgrs, h3, s2). Internally the `geohash` crate uses
//! `geo_types::Coord { x: lng, y: lat }`, which we adapt at the API
//! boundary to keep the SQL surface consistent.
//!
//! ### NULL handling
//!
//! NULL / non-coercible / unparseable input returns NULL on every
//! lookup (decode and neighbors); out-of-range lat/lng (|lat| > 90 or
//! |lng| > 180) and out-of-range precision also return NULL rather
//! than producing a malformed hash. Callers can
//! `COALESCE(geohash_encode(...), '')` instead of catching errors.

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

    use geohash::{decode, encode, neighbors, Coord, Direction, Neighbors};
    // Direction is brought into scope so the FID_NEIGHBORS arm can
    // reference Direction::N etc symbolically (though we delegate to
    // the higher-level `neighbors()` helper here).
    #[allow(unused_imports)]
    use Direction::*;

    // ─────────────── FIDs ───────────────
    const FID_ENCODE: u64 = 1;
    const FID_DECODE: u64 = 2;
    const FID_NEIGHBORS: u64 = 3;
    const FID_VERSION: u64 = 4;

    /// Geohash precision range. The geohash crate caps at 12 (the f64
    /// mantissa runs out beyond that), with a minimum of 1.
    const MIN_PRECISION: i64 = 1;
    const MAX_PRECISION: i64 = 12;
    /// Default precision 9 (~5m cell). Standard street-level GPS
    /// precision; matches Wikipedia / Niemeyer's reference docs and
    /// the brief's "Default precision 9 (~5m)" requirement.
    const DEFAULT_PRECISION: usize = 9;

    struct Ext;

    /// Coerce a SqlValue to f64 for lat/lng args. Integer / real are
    /// accepted as-is; everything else (TEXT / BLOB / NULL) propagates
    /// None so the caller can return NULL.
    fn as_f64(v: &SqlValue) -> Option<f64> {
        match v {
            SqlValue::Integer(n) => Some(*n as f64),
            SqlValue::Real(r) => Some(*r),
            _ => None,
        }
    }

    /// Coerce a SqlValue to a &str for geohash text args. BLOB is
    /// rejected -- a binary blob is never a valid geohash string, and
    /// we don't want to mis-encode UTF-8.
    fn as_text(v: &SqlValue) -> Option<&str> {
        match v {
            SqlValue::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Parse the optional precision argument. Missing / NULL =
    /// DEFAULT_PRECISION. Integer in [1, 12] is accepted; anything
    /// else (including a REAL with a fractional component or a value
    /// outside the geohash crate's accepted range) is rejected with
    /// None so the caller doesn't silently produce a wrong-length
    /// hash.
    fn opt_precision(args: &[SqlValue], idx: usize) -> Option<usize> {
        match args.get(idx) {
            None | Some(SqlValue::Null) => Some(DEFAULT_PRECISION),
            Some(SqlValue::Integer(n)) => {
                if *n < MIN_PRECISION || *n > MAX_PRECISION {
                    None
                } else {
                    Some(*n as usize)
                }
            }
            // REAL precisions are a typo (e.g. `9.0` from JSON paths).
            // Round to nearest integer if it's exact, else reject.
            Some(SqlValue::Real(r)) => {
                let n = r.round();
                if (r - n).abs() > 1e-9
                    || n < MIN_PRECISION as f64
                    || n > MAX_PRECISION as f64
                {
                    None
                } else {
                    Some(n as usize)
                }
            }
            Some(_) => None,
        }
    }

    /// JSON-encode an f64 with enough precision to round-trip the
    /// underlying value cleanly without scientific notation noise.
    /// Geohash decode gives at most ~12 chars of precision -> ~6
    /// decimal digits of lat error, so 12 fractional digits is
    /// comfortably sub-mm.
    fn fmt_coord(x: f64) -> String {
        // Use {:?} (Debug for f64) which produces the shortest
        // round-trippable representation per Rust's grisu/dragon
        // algorithm -- equivalent to JSON.stringify(x).
        format!("{:?}", x)
    }

    /// Build a JSON object from the 8 neighbor strings. We hand-roll
    /// instead of pulling serde_json in for a single 8-key object.
    /// All values are geohash strings (base-32 alphabet, ASCII only)
    /// so no escaping is needed -- the alphabet excludes ", \, control
    /// chars by construction.
    fn neighbors_to_json(n: &Neighbors) -> String {
        format!(
            "{{\"n\":\"{}\",\"ne\":\"{}\",\"e\":\"{}\",\"se\":\"{}\",\"s\":\"{}\",\"sw\":\"{}\",\"w\":\"{}\",\"nw\":\"{}\"}}",
            n.n, n.ne, n.e, n.se, n.s, n.sw, n.w, n.nw
        )
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "geohash".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // num_args = -1 advertises a variadic surface so
                    // the optional precision arg is callable.
                    s(FID_ENCODE, "geohash_encode", -1, det),
                    s(FID_DECODE, "geohash_decode", 1, det),
                    s(FID_NEIGHBORS, "geohash_neighbors", 1, det),
                    s(FID_VERSION, "geohash_version", 0, det),
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
                FID_ENCODE => {
                    let lat = match args.first().and_then(as_f64) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let lng = match args.get(1).and_then(as_f64) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    if !lat.is_finite() || !lng.is_finite() {
                        return Ok(SqlValue::Null);
                    }
                    let precision = match opt_precision(&args, 2) {
                        Some(p) => p,
                        None => return Ok(SqlValue::Null),
                    };
                    // geo_types::Coord uses x=lng, y=lat -- the
                    // mathematical convention, opposite the SQL arg
                    // order. encode() does its own range-check
                    // (|lat|<=90, |lng|<=180) and returns Err on
                    // violation; we map that to NULL.
                    let coord = Coord { x: lng, y: lat };
                    match encode(coord, precision) {
                        Ok(s) => Ok(SqlValue::Text(s)),
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_DECODE => {
                    let s = match args.first().and_then(as_text) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    // Empty hash is rejected by the crate's bbox path
                    // (length 0 leads to a zero-bit decode that
                    // collapses to (0, 0)); we explicitly reject to
                    // mirror the unparseable-input convention.
                    if s.is_empty() {
                        return Ok(SqlValue::Null);
                    }
                    match decode(s) {
                        Ok((coord, _lng_err, _lat_err)) => {
                            // JSON array [lat, lng] -- the brief
                            // requires lat first.
                            Ok(SqlValue::Text(format!(
                                "[{},{}]",
                                fmt_coord(coord.y),
                                fmt_coord(coord.x)
                            )))
                        }
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_NEIGHBORS => {
                    let s = match args.first().and_then(as_text) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    if s.is_empty() {
                        return Ok(SqlValue::Null);
                    }
                    match neighbors(s) {
                        Ok(n) => Ok(SqlValue::Text(neighbors_to_json(&n))),
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_VERSION => {
                    // The geohash crate version is pinned in
                    // Cargo.toml at "0.13" -- the actual resolved
                    // version is 0.13.1 / 0.13.2 etc but we report
                    // the requested range to keep this stable across
                    // patch bumps.
                    let v = format!(
                        "geohash ext {}; geohash 0.13",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }
                other => Err(format!("geohash: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
