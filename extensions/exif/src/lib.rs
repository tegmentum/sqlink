//! EXIF metadata extraction from JPEG / TIFF / PNG / HEIF / WebP
//! blobs via the `kamadak-exif` 0.6 pure-rust parser.
//!
//! Complements `image-meta` (dimensions + container detection only).
//! Where `image-meta` answers "what's the size", this surface answers
//! "what's the camera, when was the shot, where was the shot".
//!
//! Function surface (PLAN-more-extensions-3.md  4):
//!
//!   exif_field(blob, tag_name)  -> TEXT    raw display value for any tag
//!   exif_datetime(blob)         -> TEXT    ISO 8601 of DateTimeOriginal
//!   exif_camera(blob)           -> TEXT    "Make Model"
//!   exif_make(blob)             -> TEXT
//!   exif_model(blob)            -> TEXT
//!   exif_gps_lat(blob)          -> REAL    signed decimal degrees
//!   exif_gps_lng(blob)          -> REAL    signed decimal degrees
//!   exif_orientation(blob)      -> INTEGER 18 per EXIF spec
//!   exif_iso(blob)              -> INTEGER ISO speed
//!   exif_aperture(blob)         -> REAL    f-number
//!   exif_shutter_speed(blob)    -> TEXT    e.g. "1/250" or "0.5"
//!   exif_focal_length(blob)     -> REAL    mm
//!   exif_all(blob)              -> TEXT    JSON object of every tag
//!   exif_version()              -> TEXT
//!
//! NULL contract: every accessor returns SQL NULL on
//!   - SqlValue::Null input
//!   - non-BLOB / non-TEXT input
//!   - blobs that fail to parse as a known image container (random
//!     bytes, malformed JPEGs)
//!   - PNGs / other containers that carry no EXIF segment
//!   - the requested tag being absent from a successfully-parsed blob
//!
//! Errors are NEVER surfaced to SQL  the plan is explicit that
//! every fn must return NULL on bad input, mirroring the `image-meta`
//! convention. Each call re-parses the blob fresh; no shared state.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::fmt::Write as _;
    use std::io::Cursor;

    use exif::{Exif, In, Reader, Tag, Value};

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

    // ---- Function IDs (stable; changing breaks the loader's id->name map). ----
    const FID_FIELD: u64 = 1;
    const FID_DATETIME: u64 = 2;
    const FID_CAMERA: u64 = 3;
    const FID_MAKE: u64 = 4;
    const FID_MODEL: u64 = 5;
    const FID_GPS_LAT: u64 = 6;
    const FID_GPS_LNG: u64 = 7;
    const FID_ORIENTATION: u64 = 8;
    const FID_ISO: u64 = 9;
    const FID_APERTURE: u64 = 10;
    const FID_SHUTTER_SPEED: u64 = 11;
    const FID_FOCAL_LENGTH: u64 = 12;
    const FID_ALL: u64 = 13;
    const FID_VERSION: u64 = 14;

    struct Ext;

    // ---- Input coercion ----
    //
    // Per the NULL contract, BLOB / TEXT are the only acceptable arg-0
    // types. TEXT is treated as its raw UTF-8 byte view  callers that
    // already have the EXIF bytes hex-decoded into a TEXT column don't
    // need a CAST  X'..'.
    fn opt_bytes(args: &[SqlValue]) -> Option<Vec<u8>> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// Parse the blob with kamadak-exif's container-aware reader. The
    /// reader auto-detects TIFF / JPEG / PNG / HEIF / WebP and locates
    /// the embedded TIFF (EXIF) data inside each.
    ///
    /// Returns `None` on any error  unknown format, truncated EXIF
    /// segment, corrupt offsets  preserving the NULL-on-fail contract.
    fn parse(bytes: &[u8]) -> Option<Exif> {
        // Cursor<Vec<u8>> implements BufRead + Seek which is what
        // read_from_container requires. We clone the byte view rather
        // than seek a borrow because kamadak-exif's reader consumes
        // bytes via `chain` internally.
        let mut cur = Cursor::new(bytes.to_vec());
        Reader::new().read_from_container(&mut cur).ok()
    }

    // ---- Tag value extractors ----
    //
    // Each accessor below pulls a specific tag, then maps its value
    // to the SQL surface type. `In::PRIMARY` is the 0th IFD  the
    // image's primary metadata block. Thumbnails live in IFD 1 and
    // are intentionally NOT consulted (callers want the photo's
    // metadata, not the thumbnail's stub).

    /// Read an ASCII tag (Make / Model / DateTimeOriginal). Returns
    /// the UTF-8 string with any trailing NULs stripped. Non-ASCII
    /// variants -> None (a misencoded ASCII tag is "not present" for
    /// our purposes).
    fn ascii_tag(exif: &Exif, tag: Tag) -> Option<String> {
        let field = exif.get_field(tag, In::PRIMARY)?;
        match &field.value {
            Value::Ascii(vec) => {
                let bytes = vec.first()?;
                // The EXIF spec stores ASCII tags NUL-terminated;
                // kamadak-exif may or may not strip the NUL depending
                // on whether the source was well-formed. Trim both
                // trailing NULs and trailing whitespace just in case.
                let s = core::str::from_utf8(bytes).ok()?;
                Some(s.trim_end_matches('\0').trim().to_string())
            }
            _ => None,
        }
    }

    /// Read a tag as an unsigned integer (BYTE / SHORT / LONG).
    fn uint_tag(exif: &Exif, tag: Tag) -> Option<u32> {
        exif.get_field(tag, In::PRIMARY)?.value.get_uint(0)
    }

    /// Pull a single Rational value (FNumber, FocalLength, etc.).
    /// Returns the rational as f64 (num/denom). Zero denominator ->
    /// None (corrupt EXIF, treat as absent).
    fn rational_tag(exif: &Exif, tag: Tag) -> Option<f64> {
        let field = exif.get_field(tag, In::PRIMARY)?;
        match &field.value {
            Value::Rational(v) => {
                let r = v.first()?;
                if r.denom == 0 {
                    return None;
                }
                Some(r.num as f64 / r.denom as f64)
            }
            Value::SRational(v) => {
                let r = v.first()?;
                if r.denom == 0 {
                    return None;
                }
                Some(r.num as f64 / r.denom as f64)
            }
            _ => None,
        }
    }

    /// EXIF DateTimeOriginal is "YYYY:MM:DD HH:MM:SS" by spec.
    /// ISO 8601 is "YYYY-MM-DDTHH:MM:SS". We rewrite by replacing
    /// the first two colons with dashes and the space with 'T'.
    /// If the format already deviates (some cameras emit dashes),
    /// we still return whatever we got rather than refuse.
    fn to_iso8601(s: &str) -> String {
        // YYYY:MM:DD HH:MM:SS -- 19 chars
        let bytes = s.as_bytes();
        if bytes.len() >= 19 && bytes[4] == b':' && bytes[7] == b':' && bytes[10] == b' ' {
            let mut out = String::with_capacity(19);
            out.push_str(&s[..4]);
            out.push('-');
            out.push_str(&s[5..7]);
            out.push('-');
            out.push_str(&s[8..10]);
            out.push('T');
            out.push_str(&s[11..19]);
            out
        } else {
            s.to_string()
        }
    }

    /// Convert a GPS coordinate (3 unsigned rationals: degrees,
    /// minutes, seconds) into signed decimal degrees. `ref_str`
    /// applies the sign: 'S' / 'W' negate, 'N' / 'E' don't.
    /// Any deviation from the 3-rational format -> None.
    fn gps_to_decimal(exif: &Exif, coord_tag: Tag, ref_tag: Tag) -> Option<f64> {
        let coord = exif.get_field(coord_tag, In::PRIMARY)?;
        let rats = match &coord.value {
            Value::Rational(v) if v.len() >= 3 => v,
            _ => return None,
        };
        let mut parts = [0f64; 3];
        for (i, r) in rats.iter().take(3).enumerate() {
            if r.denom == 0 {
                return None;
            }
            parts[i] = r.num as f64 / r.denom as f64;
        }
        let decimal = parts[0] + parts[1] / 60.0 + parts[2] / 3600.0;
        // GPSLatitudeRef / GPSLongitudeRef are ASCII single chars.
        let r_field = exif.get_field(ref_tag, In::PRIMARY)?;
        let sign = match &r_field.value {
            Value::Ascii(v) => {
                let b = v.first()?;
                match b.first().copied() {
                    Some(b'S') | Some(b's') => -1.0,
                    Some(b'W') | Some(b'w') => -1.0,
                    Some(b'N') | Some(b'n') => 1.0,
                    Some(b'E') | Some(b'e') => 1.0,
                    // Spec-violating ref byte -> assume positive.
                    _ => 1.0,
                }
            }
            // Missing ref -> assume positive (north / east).
            _ => 1.0,
        };
        Some(sign * decimal)
    }

    /// Shutter speed is exposed as a TEXT  cameras typically write
    /// ExposureTime as a RATIONAL like 1/250. We emit "1/250" when
    /// the numerator is 1 and denominator is sensible, else the
    /// decimal seconds form. ShutterSpeedValue (APEX) is a fallback
    /// when ExposureTime is missing.
    fn shutter_speed_string(exif: &Exif) -> Option<String> {
        if let Some(field) = exif.get_field(Tag::ExposureTime, In::PRIMARY) {
            if let Value::Rational(v) = &field.value {
                if let Some(r) = v.first() {
                    if r.denom == 0 {
                        return None;
                    }
                    // The spec convention is num=1 for fractional
                    // shutter; preserve "1/250" exactly. For >= 1s
                    // exposures (num >= denom), emit the decimal.
                    if r.num <= r.denom {
                        return Some(format!("{}/{}", r.num, r.denom));
                    }
                    return Some(format!("{}", r.num as f64 / r.denom as f64));
                }
            }
        }
        // Fallback: APEX shutter speed value (SRational). Convert
        // via the standard 1 / 2^Tv relation. Useful for legacy
        // cameras that only write ShutterSpeedValue.
        if let Some(field) = exif.get_field(Tag::ShutterSpeedValue, In::PRIMARY) {
            if let Value::SRational(v) = &field.value {
                if let Some(r) = v.first() {
                    if r.denom == 0 {
                        return None;
                    }
                    let tv = r.num as f64 / r.denom as f64;
                    // exposure = 1 / 2^tv
                    let exp = (-tv * core::f64::consts::LN_2).exp();
                    if exp < 1.0 && exp > 0.0 {
                        // round-trip "1/N" form for fractional secs
                        let denom = (1.0 / exp).round() as u64;
                        return Some(format!("1/{denom}"));
                    }
                    return Some(format!("{exp}"));
                }
            }
        }
        None
    }

    /// Look up an arbitrary tag by canonical name (e.g. "Make",
    /// "GPSLatitude", "DateTimeOriginal"). We iterate the field list
    /// and Display-format each tag -- this is O(n_fields) per call,
    /// but n is small (typically < 60) and avoids hardcoding the
    /// 200+ tag table.
    fn field_by_name(exif: &Exif, name: &str) -> Option<String> {
        for f in exif.fields() {
            // Tag's Display impl emits the canonical EXIF name from
            // the tag_info table when known, or "Tag(ctx, NUM)"
            // otherwise. Case-sensitive match: "Make" not "make".
            // (Callers who want fuzzy matching can do their own
            // upper/lower normalization on either side.)
            let mut tag_name = String::new();
            // Tag : Display can't fail when writing to a String.
            let _ = write!(&mut tag_name, "{}", f.tag);
            if tag_name == name {
                let mut dv = String::new();
                let _ = write!(&mut dv, "{}", f.display_value());
                return Some(dv);
            }
        }
        None
    }

    /// Build a JSON object describing every tag in the EXIF blob.
    /// Keys are the canonical EXIF tag names; values are the
    /// display strings (NOT typed). This is the human-readable dump
    /// surface  for typed access prefer the specific accessors.
    fn all_json(exif: &Exif) -> String {
        let mut out = String::from("{");
        let mut first = true;
        for f in exif.fields() {
            // Skip the internal IFD-pointer tags  they're parser
            // bookkeeping, not user-visible metadata.
            if matches!(
                f.tag,
                Tag::ExifIFDPointer | Tag::GPSInfoIFDPointer | Tag::InteropIFDPointer
            ) {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            // Tag name as key (EXIF names are JSON-safe ASCII).
            out.push('"');
            let _ = write!(&mut out, "{}", f.tag);
            out.push('"');
            out.push(':');
            // Display value as a JSON string  escape backslash and
            // double-quote, replace control chars with spaces.
            let mut dv = String::new();
            let _ = write!(&mut dv, "{}", f.display_value());
            out.push('"');
            for c in dv.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '"' => out.push_str("\\\""),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push(' '),
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        out.push('}');
        out
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Pure functions of the input blob  fully deterministic.
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "exif".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FIELD, "exif_field", 2, det),
                    s(FID_DATETIME, "exif_datetime", 1, det),
                    s(FID_CAMERA, "exif_camera", 1, det),
                    s(FID_MAKE, "exif_make", 1, det),
                    s(FID_MODEL, "exif_model", 1, det),
                    s(FID_GPS_LAT, "exif_gps_lat", 1, det),
                    s(FID_GPS_LNG, "exif_gps_lng", 1, det),
                    s(FID_ORIENTATION, "exif_orientation", 1, det),
                    s(FID_ISO, "exif_iso", 1, det),
                    s(FID_APERTURE, "exif_aperture", 1, det),
                    s(FID_SHUTTER_SPEED, "exif_shutter_speed", 1, det),
                    s(FID_FOCAL_LENGTH, "exif_focal_length", 1, det),
                    s(FID_ALL, "exif_all", 1, det),
                    s(FID_VERSION, "exif_version", 0, det),
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
                preferred_prefix: Some("exif".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.exif".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            // For every blob-consuming fn, decoding failures collapse
            // to NULL via the same opt_bytes / parse(...) pair. Only
            // FID_VERSION ignores the input.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(format!(
                    "kamadak-exif 0.6; extension {}",
                    env!("CARGO_PKG_VERSION")
                )));
            }

            let Some(bytes) = opt_bytes(&args) else {
                return Ok(SqlValue::Null);
            };
            let Some(exif) = parse(&bytes) else {
                return Ok(SqlValue::Null);
            };

            match func_id {
                FID_FIELD => {
                    // exif_field(blob, tag_name) -> TEXT. NULL on
                    // missing-tag or non-TEXT tag_name arg.
                    let name = match args.get(1) {
                        Some(SqlValue::Text(s)) => s.clone(),
                        _ => return Ok(SqlValue::Null),
                    };
                    match field_by_name(&exif, &name) {
                        Some(v) => Ok(SqlValue::Text(v)),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_DATETIME => match ascii_tag(&exif, Tag::DateTimeOriginal) {
                    Some(s) => Ok(SqlValue::Text(to_iso8601(&s))),
                    None => Ok(SqlValue::Null),
                    // PLAN-wit-value-extension.md Phase A: the sql-value variant
                    // gained a wit-value arm; Phase B will replace this wildcard
                    // with extension-specific decode/encode logic.
                    _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                },
                FID_CAMERA => {
                    let make = ascii_tag(&exif, Tag::Make);
                    let model = ascii_tag(&exif, Tag::Model);
                    match (make, model) {
                        (Some(m), Some(d)) => Ok(SqlValue::Text(format!("{m} {d}"))),
                        (Some(m), None) => Ok(SqlValue::Text(m)),
                        (None, Some(d)) => Ok(SqlValue::Text(d)),
                        (None, None) => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_MAKE => match ascii_tag(&exif, Tag::Make) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                    // PLAN-wit-value-extension.md Phase A: the sql-value variant
                    // gained a wit-value arm; Phase B will replace this wildcard
                    // with extension-specific decode/encode logic.
                    _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                },
                FID_MODEL => match ascii_tag(&exif, Tag::Model) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                    // PLAN-wit-value-extension.md Phase A: the sql-value variant
                    // gained a wit-value arm; Phase B will replace this wildcard
                    // with extension-specific decode/encode logic.
                    _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                },
                FID_GPS_LAT => {
                    match gps_to_decimal(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef) {
                        Some(d) => Ok(SqlValue::Real(d)),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_GPS_LNG => {
                    match gps_to_decimal(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef) {
                        Some(d) => Ok(SqlValue::Real(d)),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_ORIENTATION => match uint_tag(&exif, Tag::Orientation) {
                    Some(n) if (1..=8).contains(&n) => Ok(SqlValue::Integer(n as i64)),
                    // Out-of-range values are corrupt per the spec
                    // (Orientation is a SHORT in 1..=8); collapse to
                    // NULL rather than expose garbage to callers.
                    _ => Ok(SqlValue::Null),
                },
                FID_ISO => {
                    // ISO can live under PhotographicSensitivity
                    // (preferred, EXIF 2.3+) or ISOSpeed
                    // (legacy/EXIF 2.3 specific tag). Try the
                    // common one first.
                    if let Some(n) = uint_tag(&exif, Tag::PhotographicSensitivity) {
                        return Ok(SqlValue::Integer(n as i64));
                    }
                    if let Some(n) = uint_tag(&exif, Tag::ISOSpeed) {
                        return Ok(SqlValue::Integer(n as i64));
                    }
                    Ok(SqlValue::Null)
                }
                FID_APERTURE => match rational_tag(&exif, Tag::FNumber) {
                    Some(v) => Ok(SqlValue::Real(v)),
                    // Fallback: ApertureValue (APEX Av). Av -> F via
                    // F = sqrt(2)^Av. Useful when FNumber is absent.
                    None => match rational_tag(&exif, Tag::ApertureValue) {
                        Some(av) => Ok(SqlValue::Real(
                            (av * 0.5_f64 * core::f64::consts::LN_2).exp(),
                        )),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    },
                    // PLAN-wit-value-extension.md Phase A: the sql-value variant
                    // gained a wit-value arm; Phase B will replace this wildcard
                    // with extension-specific decode/encode logic.
                    _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                },
                FID_SHUTTER_SPEED => match shutter_speed_string(&exif) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                    // PLAN-wit-value-extension.md Phase A: the sql-value variant
                    // gained a wit-value arm; Phase B will replace this wildcard
                    // with extension-specific decode/encode logic.
                    _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                },
                FID_FOCAL_LENGTH => match rational_tag(&exif, Tag::FocalLength) {
                    Some(v) => Ok(SqlValue::Real(v)),
                    None => Ok(SqlValue::Null),
                    // PLAN-wit-value-extension.md Phase A: the sql-value variant
                    // gained a wit-value arm; Phase B will replace this wildcard
                    // with extension-specific decode/encode logic.
                    _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                },
                FID_ALL => Ok(SqlValue::Text(all_json(&exif))),
                other => Err(format!("exif: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
