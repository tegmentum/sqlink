//! Universal Transverse Mercator (UTM) coordinate extension for SQLite.
//!
//! UTM projects the WGS84 ellipsoid onto a series of 60 6-degree
//! transverse Mercator zones, producing rectangular metric (easting,
//! northing) coordinates. It is the projection underlying MGRS but
//! exposes raw numbers instead of grid strings -- useful for survey
//! / GIS pipelines that want metric distance math directly.
//!
//! Function surface (per task brief):
//!
//!   utm_from_latlng(lat, lng) -> text
//!     Project a WGS84 lat/lng to UTM. Returns a JSON object:
//!     `{"zone":18,"hemisphere":"N","easting":583960.4,"northing":4507523.4}`.
//!     Latitude must be in [-80, 84]; outside that range UPS is used
//!     in lieu of UTM (see the `mgrs` extension for polar work) and
//!     this function returns NULL.
//!
//!   utm_to_latlng(zone, hemisphere, easting, northing) -> text
//!     Inverse projection. `hemisphere` is `'N'` or `'S'`
//!     (case-insensitive). Returns a JSON array `[lat, lng]`.
//!     Invalid zone / hemisphere / easting / northing returns NULL.
//!
//!   utm_zone_letter(lat) -> text
//!     UTM latitude-band letter (`C..X`, skipping `I` and `O`) for
//!     the given lat. Lat outside [-80, 84] returns NULL.
//!
//!   utm_zone_number(lng) -> integer
//!     UTM zone number (1..=60) for the given longitude.
//!     Longitude outside [-180, 180] returns NULL.
//!     NOTE: at high latitudes near Norway / Svalbard a few zones
//!     are offset by special rules; the brief surface is lat-less
//!     so we use the canonical longitude-only formula here.
//!
//!   utm_version() -> text
//!     `utm ext <crate-version>; utm <crate-version>`.
//!
//! ## NULL handling
//!
//! NULL / non-coercible / out-of-domain input returns NULL on every
//! lookup rather than raising. Callers can `COALESCE(utm_from_latlng(
//! lat, lng), '')` to short-circuit polar coords or bad data.
//!
//! ## Hemisphere convention
//!
//! The underlying `utm` crate distinguishes hemisphere via the zone
//! latitude-band letter (N..X = northern; C..M = southern). This
//! surface returns the simpler `'N'` / `'S'` glyph (UTM convention)
//! in JSON output; on input we accept either case.

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

    use utm::{
        lat_lon_to_zone_number, lat_to_zone_letter, to_utm_wgs84, wsg84_utm_to_lat_lon,
    };

    // ─────────────── FIDs ───────────────
    const FID_FROM_LATLNG: u64 = 1;
    const FID_TO_LATLNG: u64 = 2;
    const FID_ZONE_LETTER: u64 = 3;
    const FID_ZONE_NUMBER: u64 = 4;
    const FID_VERSION: u64 = 5;

    struct Ext;

    /// Coerce a SqlValue to f64 for lat/lng/easting/northing args.
    /// Integer / real are accepted; TEXT / BLOB / NULL propagate None
    /// so the caller can return NULL.
    fn as_f64(v: &SqlValue) -> Option<f64> {
        match v {
            SqlValue::Integer(n) => Some(*n as f64),
            SqlValue::Real(r) => Some(*r),
            _ => None,
        }
    }

    /// Coerce a SqlValue to i64 for the zone arg. Real values that
    /// are exact integers (e.g. `18.0`) are accepted; everything
    /// else returns None.
    fn as_i64(v: &SqlValue) -> Option<i64> {
        match v {
            SqlValue::Integer(n) => Some(*n),
            SqlValue::Real(r) => {
                let n = r.round();
                if (r - n).abs() < 1e-9 {
                    Some(n as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Coerce a SqlValue to a &str. BLOB / NULL are rejected.
    fn as_text(v: &SqlValue) -> Option<&str> {
        match v {
            SqlValue::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Format a float for JSON output. Uses {:.3} (mm precision)
    /// for easting / northing -- UTM eastings span [100_000, 999_999]
    /// and northings span [0, 10_000_000] in meters, so 3 decimals
    /// is sub-mm accuracy which is well beyond what the WGS84 model
    /// supports anyway (~cm-level systematic error from the
    /// truncated series expansion).
    fn fmt_m(v: f64) -> String {
        format!("{:.3}", v)
    }

    /// Format a lat/lng float for JSON. 8 decimals is ~1.1 mm at the
    /// equator -- sub-cm accuracy, well below the WGS84 / UTM
    /// projection error.
    fn fmt_deg(v: f64) -> String {
        format!("{:.8}", v)
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
                name: "utm".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FROM_LATLNG, "utm_from_latlng", 2, det),
                    s(FID_TO_LATLNG, "utm_to_latlng", 4, det),
                    s(FID_ZONE_LETTER, "utm_zone_letter", 1, det),
                    s(FID_ZONE_NUMBER, "utm_zone_number", 1, det),
                    s(FID_VERSION, "utm_version", 0, det),
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
                    // UTM is defined only in -80..84; outside that
                    // range UPS is used (see the `mgrs` extension).
                    if lat < -80.0 || lat > 84.0 {
                        return Ok(SqlValue::Null);
                    }
                    if lng < -180.0 || lng >= 180.0 {
                        // lng == 180 is the antimeridian (zone 1
                        // boundary). The underlying utm crate would
                        // emit zone 61 here, so we reject.
                        return Ok(SqlValue::Null);
                    }
                    let zone = lat_lon_to_zone_number(lat, lng);
                    let (northing, easting, _conv) = to_utm_wgs84(lat, lng, zone);
                    let hemisphere = if lat >= 0.0 { "N" } else { "S" };
                    // Manual JSON build: no serde dep on the wasm
                    // side keeps the artifact small. The output keys
                    // are fixed and string values are safe glyphs
                    // (`N` / `S`), so no escaping is needed.
                    let json = format!(
                        "{{\"zone\":{},\"hemisphere\":\"{}\",\"easting\":{},\"northing\":{}}}",
                        zone,
                        hemisphere,
                        fmt_m(easting),
                        fmt_m(northing),
                    );
                    Ok(SqlValue::Text(json))
                }
                FID_TO_LATLNG => {
                    let zone = match args.first().and_then(as_i64) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    if zone < 1 || zone > 60 {
                        return Ok(SqlValue::Null);
                    }
                    let hemisphere = match args.get(1).and_then(as_text) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    // The underlying utm crate takes a UTM latitude
                    // band letter (C..X). We accept the simpler
                    // hemisphere glyph (N or S) at the SQL layer and
                    // map it to a band that puts us safely in the
                    // northern (N) / southern (M) hemisphere.
                    //
                    //   `N` band letter: 0 .. 8N   (northern, just above equator)
                    //   `M` band letter: 0 .. 8S   (southern, just below equator)
                    //
                    // Either choice yields the same lat/lng output
                    // because the inverse formula only branches on
                    // `letter >= 'N'` (i.e. hemisphere).
                    let band_letter = match hemisphere.trim() {
                        s if s.eq_ignore_ascii_case("N") => 'N',
                        s if s.eq_ignore_ascii_case("S") => 'M',
                        _ => return Ok(SqlValue::Null),
                    };
                    let easting = match args.get(2).and_then(as_f64) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let northing = match args.get(3).and_then(as_f64) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    if !easting.is_finite() || !northing.is_finite() {
                        return Ok(SqlValue::Null);
                    }
                    let (lat, lng) = match wsg84_utm_to_lat_lon(
                        easting,
                        northing,
                        zone as u8,
                        band_letter,
                    ) {
                        Ok(v) => v,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Text(format!(
                        "[{},{}]",
                        fmt_deg(lat),
                        fmt_deg(lng)
                    )))
                }
                FID_ZONE_LETTER => {
                    let lat = match args.first().and_then(as_f64) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    if !lat.is_finite() {
                        return Ok(SqlValue::Null);
                    }
                    match lat_to_zone_letter(lat) {
                        Some(c) => Ok(SqlValue::Text(c.to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_ZONE_NUMBER => {
                    let lng = match args.first().and_then(as_f64) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    if !lng.is_finite() || lng < -180.0 || lng >= 180.0 {
                        // lng == 180 is the antimeridian; UTM treats
                        // it as the start of zone 1 (same as -180).
                        // The underlying `utm` crate's formula
                        // `((lng+180)/6).floor()+1` returns 61 here
                        // which is out of range, so we reject.
                        return Ok(SqlValue::Null);
                    }
                    // lat_lon_to_zone_number takes a latitude purely
                    // for the Norway / Svalbard exceptions; with a
                    // neutral latitude (0.0) the formula reduces to
                    // the canonical longitude-only zone math.
                    let zone = lat_lon_to_zone_number(0.0, lng);
                    Ok(SqlValue::Integer(zone as i64))
                }
                FID_VERSION => {
                    let v = format!("utm ext {}; utm 0.1.6", env!("CARGO_PKG_VERSION"));
                    Ok(SqlValue::Text(v))
                }
                other => Err(format!("utm: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
