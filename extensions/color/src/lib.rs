//! Color conversion + WCAG luminance / contrast ratio

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
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

    const FID_TO_HEX: u64 = 1;
    const FID_TO_RGB: u64 = 2;
    const FID_LUMINANCE: u64 = 3;
    const FID_CONTRAST: u64 = 4;
    const FID_RED: u64 = 5;
    const FID_GREEN: u64 = 6;
    const FID_BLUE: u64 = 7;

    struct Ext;

    /// (r, g, b) each 0..=255. Alpha intentionally not tracked
    ///  WCAG luminance + contrast are alpha-free, and storing
    /// it would force every accessor to a 4-tuple for no win.
    type Rgb = (u8, u8, u8);

    /// Parse any of:
    ///   #rgb / #rrggbb (with or without leading `#`)
    ///   rgb(r, g, b) / rgba(r, g, b, a)  alpha ignored
    ///   named CSS color (basic 16 only)
    /// Returns None on unrecognized input. Whitespace tolerated.
    fn parse(raw: &str) -> Option<Rgb> {
        let s = raw.trim();
        if let Some(hex) = s.strip_prefix('#').or(Some(s)) {
            if let Some(rgb) = parse_hex(hex) {
                return Some(rgb);
            }
        }
        if let Some(rgb) = parse_rgb_func(s) {
            return Some(rgb);
        }
        named(s)
    }

    fn parse_hex(h: &str) -> Option<Rgb> {
        let h = h.trim_start_matches('#');
        if !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        match h.len() {
            3 => {
                let r = u8::from_str_radix(&h[0..1], 16).ok()?;
                let g = u8::from_str_radix(&h[1..2], 16).ok()?;
                let b = u8::from_str_radix(&h[2..3], 16).ok()?;
                Some((r * 17, g * 17, b * 17))
            }
            6 => {
                let r = u8::from_str_radix(&h[0..2], 16).ok()?;
                let g = u8::from_str_radix(&h[2..4], 16).ok()?;
                let b = u8::from_str_radix(&h[4..6], 16).ok()?;
                Some((r, g, b))
            }
            _ => None,
        }
    }

    fn parse_rgb_func(s: &str) -> Option<Rgb> {
        let inner = s.strip_prefix("rgba(")
            .or_else(|| s.strip_prefix("rgb("))?
            .strip_suffix(')')?;
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() != 3 && parts.len() != 4 {
            return None;
        }
        let r = parts[0].trim().parse::<u16>().ok()?.min(255) as u8;
        let g = parts[1].trim().parse::<u16>().ok()?.min(255) as u8;
        let b = parts[2].trim().parse::<u16>().ok()?.min(255) as u8;
        Some((r, g, b))
    }

    fn named(s: &str) -> Option<Rgb> {
        // CSS basic-16 palette; expansion to full 147 is a 1:1
        // table append if a real consumer asks.
        let n = s.to_ascii_lowercase();
        let pair = match n.as_str() {
            "black"   => (0, 0, 0),
            "white"   => (255, 255, 255),
            "red"     => (255, 0, 0),
            "lime"    => (0, 255, 0),
            "blue"    => (0, 0, 255),
            "yellow"  => (255, 255, 0),
            "cyan" | "aqua" => (0, 255, 255),
            "magenta" | "fuchsia" => (255, 0, 255),
            "silver"  => (192, 192, 192),
            "gray" | "grey" => (128, 128, 128),
            "maroon"  => (128, 0, 0),
            "olive"   => (128, 128, 0),
            "green"   => (0, 128, 0),
            "purple"  => (128, 0, 128),
            "teal"    => (0, 128, 128),
            "navy"    => (0, 0, 128),
            _ => return None,
        };
        Some(pair)
    }

    /// WCAG relative luminance (sRGB IEC 61966-2-1).
    /// Spec: https://www.w3.org/TR/WCAG21/#dfn-relative-luminance
    fn luminance((r, g, b): Rgb) -> f64 {
        fn c(x: u8) -> f64 {
            let v = x as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * c(r) + 0.7152 * c(g) + 0.0722 * c(b)
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
                name: "color".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TO_HEX, "color_to_hex", 1, det),
                    s(FID_TO_RGB, "color_to_rgb", 1, det),
                    s(FID_LUMINANCE, "color_luminance", 1, det),
                    s(FID_CONTRAST, "color_contrast_ratio", 2, det),
                    s(FID_RED, "color_red", 1, det),
                    s(FID_GREEN, "color_green", 1, det),
                    s(FID_BLUE, "color_blue", 1, det),
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
            if func_id == FID_CONTRAST {
                let a = arg_text(&args, 0, "color_contrast_ratio")?;
                let b = arg_text(&args, 1, "color_contrast_ratio")?;
                return Ok(match (parse(&a), parse(&b)) {
                    (Some(ca), Some(cb)) => {
                        let la = luminance(ca);
                        let lb = luminance(cb);
                        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
                        SqlValue::Real((hi + 0.05) / (lo + 0.05))
                    }
                    _ => SqlValue::Null,
                });
            }

            let raw = arg_text(&args, 0, "color")?;
            let parsed = parse(&raw);
            match func_id {
                FID_TO_HEX => Ok(parsed
                    .map(|(r, g, b)| SqlValue::Text(format!("#{r:02x}{g:02x}{b:02x}")))
                    .unwrap_or(SqlValue::Null)),
                FID_TO_RGB => Ok(parsed
                    .map(|(r, g, b)| SqlValue::Text(format!("rgb({r}, {g}, {b})")))
                    .unwrap_or(SqlValue::Null)),
                FID_LUMINANCE => Ok(parsed
                    .map(|rgb| SqlValue::Real(luminance(rgb)))
                    .unwrap_or(SqlValue::Null)),
                FID_RED => Ok(parsed
                    .map(|(r, _, _)| SqlValue::Integer(r as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_GREEN => Ok(parsed
                    .map(|(_, g, _)| SqlValue::Integer(g as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_BLUE => Ok(parsed
                    .map(|(_, _, b)| SqlValue::Integer(b as i64))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("color: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
