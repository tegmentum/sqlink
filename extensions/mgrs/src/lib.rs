//! Military Grid Reference System (MGRS) extension for SQLite.
//!
//! Pairs with `h3` / `s2` (discrete global grids) by providing the
//! human-readable rectangular grid used by the DoD, NATO and SAR /
//! emergency-services tooling. Where lat/lng is a continuous
//! coordinate and h3/s2 quantize the globe into hex / quadrilateral
//! cells, MGRS is a string of the form `<zone><band> <100km-square>
//! <easting> <northing>` that humans can read off a paper map.
//!
//! Function surface (PLAN-more-extensions-4.md #4):
//!
//!   mgrs_from_latlng(lat, lng, [precision]) -> text
//!     Encode a WGS84 lat/lng to an MGRS string. `precision` selects
//!     the easting / northing digit count: 0 = 100km square only,
//!     1 = 10km, 2 = 1km, 3 = 100m, 4 = 10m, 5 = 1m (default).
//!     Output is the canonical spaced form
//!     `<grid-zone> <100km-square> <easting> <northing>`,
//!     e.g. `31U DQ 48251 11553`.
//!
//!   mgrs_to_latlng(mgrs) -> text
//!     Decode an MGRS string to `lat,lng`. Accepts both the spaced
//!     and the compact (no-space) forms and is case-insensitive.
//!
//!   mgrs_grid_zone(mgrs) -> text
//!     Pull the grid zone + latitude band prefix (e.g. `31U`).
//!
//!   mgrs_is_valid(s) -> integer (0 / 1)
//!     1 iff the string parses as MGRS.
//!
//!   mgrs_precision(mgrs) -> integer
//!     Decoded precision (0..=5; meters of accuracy =
//!     10 ^ (5 - precision)).
//!
//!   mgrs_version() -> text
//!     `mgrs ext <crate-version>; geoconvert <crate-version>`.
//!
//! ## Polar regions
//!
//! The MGRS UTM grid covers `80S` .. `84N`. Outside this band the
//! Universal Polar Stereographic (UPS) projection is used; this
//! extension delegates to the `geoconvert` crate, which handles UPS
//! transparently. Output strings in UPS regions begin with a polar
//! band letter (`A` / `B` / `Y` / `Z`) and no numeric zone prefix,
//! per the DoD spec. Callers requiring high-precision polar work
//! should still validate output independently; the canonical MGRS
//! reference is GeographicLib + DMA TM 8358.1.
//!
//! ## NULL handling
//!
//! NULL / non-coercible / unparseable input returns NULL on every
//! lookup; out-of-range lat/lng (|lat| > 90 or |lng| > 180) also
//! returns NULL rather than producing a malformed grid. Callers can
//! `COALESCE(mgrs_from_latlng(...), '')` instead of catching errors.

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

    use geoconvert::{LatLon, Mgrs};

    // ─────────────── FIDs ───────────────
    const FID_FROM_LATLNG: u64 = 1;
    const FID_TO_LATLNG: u64 = 2;
    const FID_GRID_ZONE: u64 = 3;
    const FID_IS_VALID: u64 = 4;
    const FID_PRECISION: u64 = 5;
    const FID_VERSION: u64 = 6;

    /// MGRS precision argument range. 0 = 100km square (no digits);
    /// 5 = 1m accuracy. The underlying geoconvert API uses 1..=11 but
    /// the SQL-side convention (matching the `mgrs` CLI + DoD spec
    /// summary) caps at 5 -- 1m is finer than any survey-grade GPS
    /// fix, and precisions 6..=11 are subdivisions defined by
    /// extensions in TM 8358.1 that few consumers care about.
    const MAX_SQL_PRECISION: i64 = 5;
    const DEFAULT_PRECISION: i32 = 5;

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

    /// Coerce a SqlValue to a &str for MGRS / text args. BLOB is
    /// rejected -- a binary blob is never a valid MGRS grid string,
    /// and we don't want to mis-encode UTF-8.
    fn as_text(v: &SqlValue) -> Option<&str> {
        match v {
            SqlValue::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Pull the optional precision argument. Missing / NULL =
    /// DEFAULT_PRECISION. Integer in [0, 5] is accepted; anything
    /// else is rejected with NULL so the caller doesn't silently
    /// produce a wrong-precision string.
    fn opt_precision(args: &[SqlValue], idx: usize) -> Option<i32> {
        match args.get(idx) {
            None | Some(SqlValue::Null) => Some(DEFAULT_PRECISION),
            Some(SqlValue::Integer(n)) => {
                if *n < 0 || *n > MAX_SQL_PRECISION {
                    None
                } else {
                    Some(*n as i32)
                }
            }
            // REAL precisions are a typo (e.g. `3.0` from JSON paths).
            // Round to nearest integer if it's exact, else reject.
            Some(SqlValue::Real(r)) => {
                let n = r.round();
                if (r - n).abs() > 1e-9 || n < 0.0 || n > MAX_SQL_PRECISION as f64 {
                    None
                } else {
                    Some(n as i32)
                }
            }
            Some(_) => None,
        }
    }

    /// Format an `Mgrs` value as a canonical spaced MGRS string:
    ///   <zone><band> <100km-square> <easting> <northing>
    /// e.g. `31U DQ 48251 11553`. UPS strings (polar regions) have
    /// no numeric zone, so the first chunk is just the polar band +
    /// 100km-square: `A AB 12345 67890`.
    ///
    /// The geoconvert `Display` impl emits the compact form
    /// `31UDQ4825111553`; this routine splits it into the canonical
    /// 4-chunk presentation. The compact form is what's accepted on
    /// input.
    fn format_mgrs(mgrs: &Mgrs) -> String {
        let s = mgrs.to_string();
        let utm = mgrs.is_utm();
        let bytes = s.as_bytes();

        // UTM: 2 digits + 1 latband + 2 100km letters + 2*precision digits
        // UPS: 1 polar band + 2 100km letters + 2*precision digits
        // mgrs.precision() is the digit count (0..=5 we expose).
        let prec = mgrs.precision().max(0) as usize;
        let head_len = if utm { 3 } else { 1 }; // zone+band or polar-band
        let square_len = 2; // 100km-square letters
        let digits_total = prec * 2;

        // Defensive: if the encoded string is shorter than expected
        // (precision == -1 or 0 -- "grid square only"), just emit
        // head + square.
        if bytes.len() < head_len + square_len {
            // Should not normally happen; fall back to raw.
            return s;
        }

        let head = &s[..head_len];
        let square = &s[head_len..head_len + square_len];

        if digits_total == 0 || bytes.len() < head_len + square_len + digits_total {
            return format!("{} {}", head, square);
        }

        let digits = &s[head_len + square_len..head_len + square_len + digits_total];
        let (e, n) = digits.split_at(prec);
        format!("{} {} {} {}", head, square, e, n)
    }

    /// Pull the grid-zone prefix from a (compact-form) MGRS string.
    /// UTM grids: `<1-2 digits><1 letter>` (e.g. `31U`, `4Q`).
    /// UPS grids: `<1 letter>` (polar band).
    ///
    /// geoconvert always emits zones as 2-digit-zero-padded
    /// (`04U`, `31U`, ...). The DoD MGRS canonical form drops the
    /// leading zero for single-digit zones, so we strip it on the
    /// way out.
    fn grid_zone_prefix(mgrs: &Mgrs) -> String {
        let s = mgrs.to_string();
        if mgrs.is_utm() {
            // First 3 ASCII chars are <zone-digits><latband>.
            let raw: String = s.chars().take(3).collect();
            // raw[0..2] is the zone string, raw[2] is the band letter.
            let zone = mgrs.zone();
            let band: char = raw.chars().nth(2).unwrap_or('?');
            if zone < 10 {
                format!("{}{}", zone, band)
            } else {
                raw
            }
        } else {
            s.chars().take(1).collect()
        }
    }

    /// Normalize input MGRS string: strip whitespace + uppercase.
    /// The underlying `parse_str` insists on no-space input.
    fn normalize_input(s: &str) -> String {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_ascii_uppercase())
            .collect()
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
                name: "mgrs".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // num_args = -1 advertises a variadic surface so
                    // the optional precision arg is callable.
                    s(FID_FROM_LATLNG, "mgrs_from_latlng", -1, det),
                    s(FID_TO_LATLNG, "mgrs_to_latlng", 1, det),
                    s(FID_GRID_ZONE, "mgrs_grid_zone", 1, det),
                    s(FID_IS_VALID, "mgrs_is_valid", 1, det),
                    s(FID_PRECISION, "mgrs_precision", 1, det),
                    s(FID_VERSION, "mgrs_version", 0, det),
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
            match func_id {
                FID_FROM_LATLNG => {
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
                    let latlon = match LatLon::create(lat, lng) {
                        Ok(v) => v,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    // geoconvert::Mgrs requires precision in [1, 11].
                    // For our SQL precision == 0 (100km square only)
                    // we ask geoconvert for precision 1 and trim back
                    // when formatting.
                    let geo_precision = if precision == 0 { 1 } else { precision };
                    let mut mgrs = Mgrs::from_latlon(&latlon, geo_precision);
                    if precision == 0 {
                        // Patch the precision field so format_mgrs
                        // emits only the head + 100km square.
                        // geoconvert validates set_precision in
                        // [1, 11], so we re-encode without it: just
                        // emit the grid-square chunk by hand.
                        let raw = mgrs.to_string();
                        let utm = mgrs.is_utm();
                        let head_len = if utm { 3 } else { 1 };
                        if raw.len() < head_len + 2 {
                            return Ok(SqlValue::Null);
                        }
                        let head = &raw[..head_len];
                        let square = &raw[head_len..head_len + 2];
                        return Ok(SqlValue::Text(format!("{} {}", head, square)));
                    }
                    // Round-trip through set_precision so format_mgrs
                    // can rely on precision() being our value.
                    let _ = mgrs.set_precision(precision);
                    Ok(SqlValue::Text(format_mgrs(&mgrs)))
                }
                FID_TO_LATLNG => {
                    let s = match args.first().and_then(as_text) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let normalized = normalize_input(s);
                    let mgrs = match Mgrs::parse_str(&normalized) {
                        Ok(v) => v,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    let latlon = mgrs.to_latlon();
                    // Format as `lat,lng`; 8 decimals is sub-cm for
                    // wgs84 lat/lng (~1.1 mm at the equator).
                    Ok(SqlValue::Text(format!(
                        "{:.8},{:.8}",
                        latlon.latitude(),
                        latlon.longitude()
                    )))
                }
                FID_GRID_ZONE => {
                    let s = match args.first().and_then(as_text) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let normalized = normalize_input(s);
                    let mgrs = match Mgrs::parse_str(&normalized) {
                        Ok(v) => v,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Text(grid_zone_prefix(&mgrs)))
                }
                FID_IS_VALID => {
                    let s = match args.first().and_then(as_text) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Integer(0)),
                    };
                    let normalized = normalize_input(s);
                    let ok = Mgrs::parse_str(&normalized).is_ok();
                    Ok(SqlValue::Integer(if ok { 1 } else { 0 }))
                }
                FID_PRECISION => {
                    let s = match args.first().and_then(as_text) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let normalized = normalize_input(s);
                    let mgrs = match Mgrs::parse_str(&normalized) {
                        Ok(v) => v,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    // geoconvert::Mgrs::parse_str returns precision
                    // == -1 for "grid-square only" inputs (no
                    // easting/northing digits). Map that to our SQL
                    // convention of 0.
                    let p = mgrs.precision();
                    let p_sql = if p < 0 { 0 } else { p } as i64;
                    Ok(SqlValue::Integer(p_sql))
                }
                FID_VERSION => {
                    let v = format!(
                        "mgrs ext {}; geoconvert 1.0",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }
                other => Err(format!("mgrs: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
