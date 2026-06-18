//! Google polyline coord encoding

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

    const FID_ENCODE: u64 = 1;
    const FID_DECODE: u64 = 2;
    const FID_LENGTH: u64 = 3;

    struct Ext;

    /// Google polyline format. Each coordinate as signed int with the
    /// previous coordinate subtracted (delta encoded); the result is
    /// shifted left 1 bit, inverted if negative, then chunked into
    /// 5-bit groups, OR'd with 0x20 for continuation, plus 63 to
    /// land in printable ASCII.
    /// Reference: https://developers.google.com/maps/documentation/utilities/polylinealgorithmformat

    fn encode_value(mut v: i32, out: &mut String) {
        // Shift sign bit
        let mut sv: u32 = (v << 1) as u32;
        if v < 0 {
            sv = !sv;
        }
        // Drop unused; satisfy borrow checker.
        let _ = &mut v;
        loop {
            let chunk = (sv & 0x1F) as u8;
            sv >>= 5;
            let byte = if sv > 0 { chunk | 0x20 } else { chunk } + 63;
            out.push(byte as char);
            if sv == 0 {
                break;
            }
        }
    }

    /// Decode one signed value from `bytes` starting at `cursor`.
    /// Advances `cursor` past the value's bytes. Returns the value
    /// or None on truncated input.
    fn decode_value(bytes: &[u8], cursor: &mut usize) -> Option<i32> {
        let mut result: u32 = 0;
        let mut shift: u32 = 0;
        loop {
            if *cursor >= bytes.len() {
                return None;
            }
            let b = bytes[*cursor];
            if b < 63 {
                return None;
            }
            let chunk = b - 63;
            *cursor += 1;
            result |= ((chunk & 0x1F) as u32) << shift;
            if (chunk & 0x20) == 0 {
                break;
            }
            shift += 5;
            if shift > 30 {
                return None;  // overflow
            }
        }
        let signed = if result & 1 != 0 {
            !(result >> 1) as i32
        } else {
            (result >> 1) as i32
        };
        Some(signed)
    }

    /// Encode a sequence of (lat, lon) pairs at precision 5
    /// (Google's default). Each coord scaled by 1e5 before delta-
    /// encoding.
    fn encode(coords: &[(f64, f64)]) -> String {
        let mut out = String::new();
        let mut prev_lat: i32 = 0;
        let mut prev_lon: i32 = 0;
        for (lat, lon) in coords {
            let scaled_lat = (lat * 1e5).round() as i32;
            let scaled_lon = (lon * 1e5).round() as i32;
            encode_value(scaled_lat - prev_lat, &mut out);
            encode_value(scaled_lon - prev_lon, &mut out);
            prev_lat = scaled_lat;
            prev_lon = scaled_lon;
        }
        out
    }

    fn decode(s: &str) -> Option<Vec<(f64, f64)>> {
        let bytes = s.as_bytes();
        let mut cursor = 0;
        let mut out: Vec<(f64, f64)> = alloc::vec![];
        let mut lat: i32 = 0;
        let mut lon: i32 = 0;
        while cursor < bytes.len() {
            let dlat = decode_value(bytes, &mut cursor)?;
            let dlon = decode_value(bytes, &mut cursor)?;
            lat += dlat;
            lon += dlon;
            out.push((lat as f64 / 1e5, lon as f64 / 1e5));
        }
        Some(out)
    }

    fn parse_coords(s: &str) -> Option<Vec<(f64, f64)>> {
        use serde_json::Value;
        let v: Value = serde_json::from_str(s).ok()?;
        let arr = v.as_array()?;
        let mut out: Vec<(f64, f64)> = alloc::vec![];
        for pair in arr {
            let pair = pair.as_array()?;
            if pair.len() != 2 { return None; }
            let lat = pair[0].as_f64()?;
            let lon = pair[1].as_f64()?;
            out.push((lat, lon));
        }
        Some(out)
    }

    fn coords_to_json(coords: &[(f64, f64)]) -> String {
        let mut out = String::with_capacity(coords.len() * 20);
        out.push('[');
        for (i, (lat, lon)) in coords.iter().enumerate() {
            if i > 0 { out.push(','); }
            // Round to 5dp to match the scale we encoded with;
            // avoids 38.5 looking like 38.49999999.
            let lat5 = (lat * 1e5).round() / 1e5;
            let lon5 = (lon * 1e5).round() / 1e5;
            out.push_str(&format!("[{lat5},{lon5}]"));
        }
        out.push(']');
        out
    }

    // ---- Arg helpers ----
    // The Big Three; copy-pasted into every extension. The
    // scaffold ships them so you delete what you don't need.

    #[allow(dead_code)]
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Available flags  pass `det` for deterministic scalars
            // (most cases), `nd` for ones that produce different
            // output each call (rng / time-of-call / counter).
            #[allow(unused_variables)]
            let det = FunctionFlags::DETERMINISTIC;
            #[allow(unused_variables)]
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "google_polyline".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE, "polyline_encode", 1, det),
                    s(FID_DECODE, "polyline_decode", 1, det),
                    s(FID_LENGTH, "polyline_length", 1, det),
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
                FID_ENCODE => {
                    let s = arg_text(&args, 0, "polyline_encode")?;
                    Ok(parse_coords(&s)
                        .map(|c| SqlValue::Text(encode(&c)))
                        .unwrap_or(SqlValue::Null))
                }
                FID_DECODE => {
                    let s = arg_text(&args, 0, "polyline_decode")?;
                    Ok(decode(&s)
                        .map(|c| SqlValue::Text(coords_to_json(&c)))
                        .unwrap_or(SqlValue::Null))
                }
                FID_LENGTH => {
                    let s = arg_text(&args, 0, "polyline_length")?;
                    Ok(decode(&s)
                        .map(|c| SqlValue::Integer(c.len() as i64))
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("polyline: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
