//! NMEA 0183 GPS sentence parsing scalars.
//!
//! Wraps the `nmea` 0.6 crate (parse_str + parse_nmea_sentence).
//! Every parser scalar returns NULL on parse failure / checksum
//! mismatch / unsupported sentence; explicit NULL passes through.
//!
//! Functions:
//!   nmea_sentence_type(s) -> text     e.g. "GGA", "RMC", "VTG"
//!   nmea_lat(s)           -> real     decimal degrees, signed
//!   nmea_lng(s)           -> real     decimal degrees, signed
//!   nmea_speed_knots(s)   -> real     speed over ground, knots
//!   nmea_course(s)        -> real     true course / track, degrees
//!   nmea_timestamp(s)     -> text     ISO 8601 (date-time when RMC
//!                                     supplies a date; HH:MM:SS.fff
//!                                     otherwise)
//!   nmea_fix_quality(s)   -> integer  0..8 fix-type code (GGA only)
//!   nmea_satellites(s)    -> integer  number of satellites in fix
//!   nmea_parse(s)         -> json     all extracted fields, NULLs
//!                                     for absent data
//!   nmea_checksum_ok(s)   -> integer  1 = checksum matches, 0 = not
//!   nmea_version()        -> text     this extension's crate version
//!
//! The crate's checksum is parsed from the trailing `*HH`; the
//! validation re-XORs the talker-id, message-id, and data bytes.
//! `nmea_checksum_ok` returns 0 for a malformed sentence (not NULL)
//! so callers can `WHERE nmea_checksum_ok(s) = 1` without an extra
//! IS NOT NULL guard.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use nmea::{parse_nmea_sentence, parse_str, ParseResult, SentenceType};

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

    // ---- Function IDs ----
    // Stable identifiers; do not renumber.
    const FID_SENTENCE_TYPE: u64 = 1;
    const FID_LAT: u64 = 2;
    const FID_LNG: u64 = 3;
    const FID_SPEED_KNOTS: u64 = 4;
    const FID_COURSE: u64 = 5;
    const FID_TIMESTAMP: u64 = 6;
    const FID_FIX_QUALITY: u64 = 7;
    const FID_SATELLITES: u64 = 8;
    const FID_PARSE: u64 = 9;
    const FID_CHECKSUM_OK: u64 = 10;
    const FID_VERSION: u64 = 11;

    struct Ext;

    /// TEXT-or-NULL argument shape. Any other type is a hard error
    /// so callers know they passed the wrong column.
    enum SArg {
        Null,
        Text(String),
    }

    fn arg_s(args: &[SqlValue], fname: &str) -> Result<SArg, String> {
        match args.first() {
            Some(SqlValue::Null) | None => Ok(SArg::Null),
            Some(SqlValue::Text(s)) => Ok(SArg::Text(s.clone())),
            Some(_) => Err(format!("{fname}: TEXT or NULL arg required")),
        }
    }

    /// Map GGA fix_type enum -> 0..8 code per gpsd.
    fn fix_type_code(ft: nmea::sentences::FixType) -> i64 {
        use nmea::sentences::FixType::*;
        match ft {
            Invalid => 0,
            Gps => 1,
            DGps => 2,
            Pps => 3,
            Rtk => 4,
            FloatRtk => 5,
            Estimated => 6,
            Manual => 7,
            Simulation => 8,
        }
    }

    /// Extract sentence type from a raw NMEA sentence without
    /// checksum-validating the body (which `parse_str` does). We
    /// only need talker_id + message_id, both produced before the
    /// body parsers run.
    fn sentence_type_str(s: &str) -> Option<String> {
        let parsed = parse_nmea_sentence(s).ok()?;
        Some(parsed.message_id.as_str().to_string())
    }

    /// Same idea: derive checksum-validity from talker_id +
    /// message_id + data, ignoring whether the body parsers like
    /// the sentence.
    fn checksum_matches(s: &str) -> bool {
        match parse_nmea_sentence(s) {
            Ok(ns) => ns.checksum == ns.calc_checksum(),
            Err(_) => false,
        }
    }

    /// Extract (lat, lon) from any sentence variant that carries
    /// a geographic position. Returns `(None, None)` when the
    /// sentence has no lat/lon slot at all.
    fn lat_lon_from(p: &ParseResult) -> (Option<f64>, Option<f64>) {
        match p {
            ParseResult::GGA(d) => (d.latitude, d.longitude),
            ParseResult::RMC(d) => (d.lat, d.lon),
            ParseResult::GLL(d) => (d.latitude, d.longitude),
            ParseResult::GNS(d) => (d.lat, d.lon),
            _ => (None, None),
        }
    }

    /// Speed-over-ground (knots) extractor. VTG already normalizes
    /// kph -> knots in `speed_over_ground`, so we just read the
    /// field directly. RMC reports knots natively.
    fn speed_knots_from(p: &ParseResult) -> Option<f32> {
        match p {
            ParseResult::RMC(d) => d.speed_over_ground,
            ParseResult::VTG(d) => d.speed_over_ground,
            _ => None,
        }
    }

    /// True course (degrees). RMC + VTG carry it; others return None.
    fn course_from(p: &ParseResult) -> Option<f32> {
        match p {
            ParseResult::RMC(d) => d.true_course,
            ParseResult::VTG(d) => d.true_course,
            _ => None,
        }
    }

    /// Build an ISO 8601 timestamp string. RMC carries a date so we
    /// emit `YYYY-MM-DDTHH:MM:SS[.fff]`. GGA/GLL only have a time, so
    /// we emit `HH:MM:SS[.fff]` to stay round-trippable.
    fn timestamp_from(p: &ParseResult) -> Option<String> {
        match p {
            ParseResult::RMC(d) => match (d.fix_date, d.fix_time) {
                (Some(date), Some(time)) => {
                    Some(format!("{}T{}", date.format("%Y-%m-%d"), fmt_time(&time)))
                }
                (None, Some(time)) => Some(fmt_time(&time)),
                _ => None,
            },
            ParseResult::GGA(d) => d.fix_time.as_ref().map(fmt_time),
            ParseResult::GLL(d) => Some(fmt_time(&d.fix_time)),
            ParseResult::GNS(d) => d.fix_time.as_ref().map(fmt_time),
            _ => None,
        }
    }

    /// `HH:MM:SS` if the fractional part is zero, `HH:MM:SS.fff`
    /// otherwise. Keeps the smoke output stable for whole-second
    /// timestamps without dropping precision when the device sends
    /// milliseconds.
    fn fmt_time(t: &chrono::NaiveTime) -> String {
        use chrono::Timelike;
        if t.nanosecond() == 0 {
            t.format("%H:%M:%S").to_string()
        } else {
            t.format("%H:%M:%S%.3f").to_string()
        }
    }

    /// GGA's fix_type 0..8 code (the "fix quality" NMEA column).
    /// Other sentences don't carry this column.
    fn fix_quality_from(p: &ParseResult) -> Option<i64> {
        match p {
            ParseResult::GGA(d) => d.fix_type.map(fix_type_code),
            _ => None,
        }
    }

    /// Satellite count. GGA reports it natively; GSA gives us the
    /// PRN list length, which is the number of sats *used in the
    /// fix*  the same denotation per gpsd.
    fn satellites_from(p: &ParseResult) -> Option<i64> {
        match p {
            ParseResult::GGA(d) => d.fix_satellites.map(|n| n as i64),
            ParseResult::GSA(d) => Some(d.fix_sats_prn.len() as i64),
            _ => None,
        }
    }

    fn opt_f64_json(v: Option<f64>) -> serde_json::Value {
        match v {
            Some(x) => serde_json::Number::from_f64(x)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    }

    fn opt_f32_json(v: Option<f32>) -> serde_json::Value {
        opt_f64_json(v.map(|x| x as f64))
    }

    fn opt_str_json(v: Option<String>) -> serde_json::Value {
        match v {
            Some(s) => serde_json::Value::String(s),
            None => serde_json::Value::Null,
        }
    }

    fn opt_i64_json(v: Option<i64>) -> serde_json::Value {
        match v {
            Some(n) => serde_json::Value::Number(serde_json::Number::from(n)),
            None => serde_json::Value::Null,
        }
    }

    /// JSON dump of every field this extension surfaces. Missing
    /// fields are JSON null so callers can `->>'lat'` without
    /// special-casing sentence-type per row.
    fn parse_json(s: &str) -> String {
        let mut obj = serde_json::Map::new();
        // Cheap fields first: sentence_type + checksum_ok come from
        // the bare header parse, which succeeds on a wider class of
        // inputs than parse_str (the body parser is stricter).
        let header = parse_nmea_sentence(s).ok();
        obj.insert(
            "sentence_type".to_string(),
            opt_str_json(header.as_ref().map(|h| h.message_id.as_str().to_string())),
        );
        obj.insert(
            "talker_id".to_string(),
            opt_str_json(header.as_ref().map(|h| h.talker_id.to_string())),
        );
        obj.insert(
            "checksum_ok".to_string(),
            serde_json::Value::Bool(
                header
                    .as_ref()
                    .map(|h| h.checksum == h.calc_checksum())
                    .unwrap_or(false),
            ),
        );

        // Body-level fields require parse_str to succeed.
        match parse_str(s) {
            Ok(p) => {
                let (lat, lon) = lat_lon_from(&p);
                obj.insert("lat".to_string(), opt_f64_json(lat));
                obj.insert("lng".to_string(), opt_f64_json(lon));
                obj.insert(
                    "speed_knots".to_string(),
                    opt_f32_json(speed_knots_from(&p)),
                );
                obj.insert("course".to_string(), opt_f32_json(course_from(&p)));
                obj.insert(
                    "timestamp".to_string(),
                    opt_str_json(timestamp_from(&p)),
                );
                obj.insert(
                    "fix_quality".to_string(),
                    opt_i64_json(fix_quality_from(&p)),
                );
                obj.insert(
                    "satellites".to_string(),
                    opt_i64_json(satellites_from(&p)),
                );
            }
            Err(_) => {
                for k in [
                    "lat",
                    "lng",
                    "speed_knots",
                    "course",
                    "timestamp",
                    "fix_quality",
                    "satellites",
                ] {
                    obj.insert(k.to_string(), serde_json::Value::Null);
                }
            }
        }
        serde_json::Value::Object(obj).to_string()
    }

    /// Helpers for the typed scalar functions: parse + extract, mapping
    /// every error / missing-field path to SqlValue::Null.
    fn sql_lat(s: &str) -> SqlValue {
        match parse_str(s) {
            Ok(p) => match lat_lon_from(&p).0 {
                Some(x) => SqlValue::Real(x),
                None => SqlValue::Null,
            },
            Err(_) => SqlValue::Null,
        }
    }

    fn sql_lng(s: &str) -> SqlValue {
        match parse_str(s) {
            Ok(p) => match lat_lon_from(&p).1 {
                Some(x) => SqlValue::Real(x),
                None => SqlValue::Null,
            },
            Err(_) => SqlValue::Null,
        }
    }

    fn sql_speed_knots(s: &str) -> SqlValue {
        match parse_str(s) {
            Ok(p) => match speed_knots_from(&p) {
                Some(x) => SqlValue::Real(x as f64),
                None => SqlValue::Null,
            },
            Err(_) => SqlValue::Null,
        }
    }

    fn sql_course(s: &str) -> SqlValue {
        match parse_str(s) {
            Ok(p) => match course_from(&p) {
                Some(x) => SqlValue::Real(x as f64),
                None => SqlValue::Null,
            },
            Err(_) => SqlValue::Null,
        }
    }

    fn sql_timestamp(s: &str) -> SqlValue {
        match parse_str(s) {
            Ok(p) => match timestamp_from(&p) {
                Some(t) => SqlValue::Text(t),
                None => SqlValue::Null,
            },
            Err(_) => SqlValue::Null,
        }
    }

    fn sql_fix_quality(s: &str) -> SqlValue {
        match parse_str(s) {
            Ok(p) => match fix_quality_from(&p) {
                Some(n) => SqlValue::Integer(n),
                None => SqlValue::Null,
            },
            Err(_) => SqlValue::Null,
        }
    }

    fn sql_satellites(s: &str) -> SqlValue {
        match parse_str(s) {
            Ok(p) => match satellites_from(&p) {
                Some(n) => SqlValue::Integer(n),
                None => SqlValue::Null,
            },
            Err(_) => SqlValue::Null,
        }
    }

    // Silence the unused-import lint when the SentenceType alias isn't
    // exercised by every code path the matrix throws at us.
    #[allow(dead_code)]
    fn _force_sentence_type_import(_s: SentenceType) {}

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
                name: "nmea".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_SENTENCE_TYPE, "nmea_sentence_type", 1, det),
                    s(FID_LAT, "nmea_lat", 1, det),
                    s(FID_LNG, "nmea_lng", 1, det),
                    s(FID_SPEED_KNOTS, "nmea_speed_knots", 1, det),
                    s(FID_COURSE, "nmea_course", 1, det),
                    s(FID_TIMESTAMP, "nmea_timestamp", 1, det),
                    s(FID_FIX_QUALITY, "nmea_fix_quality", 1, det),
                    s(FID_SATELLITES, "nmea_satellites", 1, det),
                    s(FID_PARSE, "nmea_parse", 1, det),
                    s(FID_CHECKSUM_OK, "nmea_checksum_ok", 1, det),
                    s(FID_VERSION, "nmea_version", 0, det),
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
            // VERSION is the only zero-arg function; handle it first
            // so we don't trip the arg_s NULL-passthrough branch.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }

            let fname = match func_id {
                FID_SENTENCE_TYPE => "nmea_sentence_type",
                FID_LAT => "nmea_lat",
                FID_LNG => "nmea_lng",
                FID_SPEED_KNOTS => "nmea_speed_knots",
                FID_COURSE => "nmea_course",
                FID_TIMESTAMP => "nmea_timestamp",
                FID_FIX_QUALITY => "nmea_fix_quality",
                FID_SATELLITES => "nmea_satellites",
                FID_PARSE => "nmea_parse",
                FID_CHECKSUM_OK => "nmea_checksum_ok",
                _ => return Err(format!("nmea: unknown func id {func_id}")),
            };

            let s = match arg_s(&args, fname)? {
                // Per the brief: NULL in -> NULL out. The single
                // exception is nmea_checksum_ok where a NULL arg
                // still NULLs out (the brief says "NULL on parse
                // failure" but a NULL input isn't a parse failure
                // - we treat it the same as everything else for
                // shape consistency).
                SArg::Null => return Ok(SqlValue::Null),
                SArg::Text(s) => s,
            };

            match func_id {
                FID_SENTENCE_TYPE => Ok(match sentence_type_str(&s) {
                    Some(t) => SqlValue::Text(t),
                    None => SqlValue::Null,
                }),
                FID_LAT => Ok(sql_lat(&s)),
                FID_LNG => Ok(sql_lng(&s)),
                FID_SPEED_KNOTS => Ok(sql_speed_knots(&s)),
                FID_COURSE => Ok(sql_course(&s)),
                FID_TIMESTAMP => Ok(sql_timestamp(&s)),
                FID_FIX_QUALITY => Ok(sql_fix_quality(&s)),
                FID_SATELLITES => Ok(sql_satellites(&s)),
                FID_PARSE => Ok(SqlValue::Text(parse_json(&s))),
                FID_CHECKSUM_OK => Ok(SqlValue::Integer(if checksum_matches(&s) { 1 } else { 0 })),
                _ => unreachable!(),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
