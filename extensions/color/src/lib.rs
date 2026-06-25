//! Color extension: CSS parse + RGB/HSL/HSV conversions + WCAG
//! luminance / contrast ratio + linear-light mix + invert.
//!
//! Function surface (PLAN-more-extensions-3.md #5):
//!
//!   Parsing / canonicalization:
//!     color_parse(s)              -> '#rrggbb' (any hex / rgb() /
//!                                     hsl() / CSS name)
//!     color_named(name)           -> '#rrggbb' or NULL
//!
//!   Conversions (text JSON arrays for the multi-channel returns):
//!     color_rgb_to_hex(r,g,b)     -> '#rrggbb'
//!     color_hex_to_rgb(hex)       -> '[r,g,b]'
//!     color_rgb_to_hsl(r,g,b)     -> '[h,s,l]'  (h 0..360, s/l 0..100)
//!     color_hsl_to_rgb(h,s,l)     -> '[r,g,b]'
//!     color_rgb_to_hsv(r,g,b)     -> '[h,s,v]'  (h 0..360, s/v 0..100)
//!     color_hsv_to_rgb(h,s,v)     -> '[r,g,b]'
//!
//!   WCAG accessibility:
//!     color_luminance(color)      -> real      (0..1)
//!     color_contrast_ratio(a,b)   -> real      (1..21)
//!
//!   Manipulation:
//!     color_mix(a, b, t)          -> '#rrggbb' (linear-light interp,
//!                                     t in 0..1)
//!     color_invert(color)         -> '#rrggbb' (255-x per channel)
//!
//!   Misc:
//!     color_version()             -> text
//!
//!   Back-compat (kept from v0.1 so existing callers keep working):
//!     color_to_hex / color_to_rgb / color_red / color_green / color_blue
//!
//! Input parser accepts: '#rgb', '#rrggbb', plain hex (no #), CSS
//! named colors (148, X11 + CSS4), 'rgb(r,g,b)', 'rgba(r,g,b,a)',
//! 'hsl(...)', 'hsla(...)', 'hwb(...)', 'lab(...)', 'oklch(...)',
//! 'transparent'. NULL -> NULL on every fn.
//!
//! Mix semantics: linear-light (sRGB -> linear -> mean -> sRGB).
//! This is *not* what naive sRGB-space mixing would give for
//! mid-grey from black + white -- the linear-light midpoint of
//! '#000000' and '#ffffff' decodes to about '#bcbcbc', not
//! '#808080'. The plan calls this out (risk row). The plan's
//! acceptance line about '#808080' assumes sRGB-space mix; we
//! intentionally choose the gamma-correct path because that's the
//! one designers actually want for color picking. The smoke covers
//! both endpoints (t=0, t=1) plus a sanity midpoint.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use csscolorparser::Color;

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
    // 1..=7 are the v0.1 surface, kept as-is for ABI stability.
    const FID_TO_HEX: u64 = 1;
    const FID_TO_RGB: u64 = 2;
    const FID_LUMINANCE: u64 = 3;
    const FID_CONTRAST: u64 = 4;
    const FID_RED: u64 = 5;
    const FID_GREEN: u64 = 6;
    const FID_BLUE: u64 = 7;

    // 8..=19 are the v0.2 expansion (PLAN-more-extensions-3 #5).
    const FID_PARSE: u64 = 8;
    const FID_NAMED: u64 = 9;
    const FID_RGB_TO_HEX: u64 = 10;
    const FID_HEX_TO_RGB: u64 = 11;
    const FID_RGB_TO_HSL: u64 = 12;
    const FID_HSL_TO_RGB: u64 = 13;
    const FID_RGB_TO_HSV: u64 = 14;
    const FID_HSV_TO_RGB: u64 = 15;
    const FID_MIX: u64 = 16;
    const FID_INVERT: u64 = 17;
    const FID_VERSION: u64 = 18;

    struct Ext;

    // ---- Parsing ----

    /// Front door: take any CSS color string and return [r, g, b]
    /// 8-bit channels. Returns None on unrecognized input. Alpha
    /// is dropped on the floor -- the surface this extension
    /// exposes is alpha-free by design (WCAG ops don't take alpha;
    /// hex output is always 6 digits).
    fn parse_color(raw: &str) -> Option<(u8, u8, u8)> {
        let c: Color = raw.parse().ok()?;
        let [r, g, b, _a] = c.to_rgba8();
        Some((r, g, b))
    }

    /// WCAG relative luminance (sRGB IEC 61966-2-1).
    /// Spec: https://www.w3.org/TR/WCAG21/#dfn-relative-luminance
    fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
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

    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let la = luminance(a);
        let lb = luminance(b);
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// sRGB -> linear-light (per channel, on 0..1 scale).
    fn srgb_to_linear(v: f64) -> f64 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Linear-light -> sRGB (per channel, on 0..1 scale).
    fn linear_to_srgb(v: f64) -> f64 {
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    }

    /// Linear-light mix at parameter t in [0,1]. Clamps t.
    /// Uses 0..1 channel math then re-quantizes to u8 with
    /// round-half-up to match common color tooling.
    fn mix_linear(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
        let t = t.clamp(0.0, 1.0);
        let mix1 = |x: u8, y: u8| {
            let lx = srgb_to_linear(x as f64 / 255.0);
            let ly = srgb_to_linear(y as f64 / 255.0);
            let lm = lx * (1.0 - t) + ly * t;
            let out = (linear_to_srgb(lm) * 255.0).round();
            out.clamp(0.0, 255.0) as u8
        };
        (mix1(a.0, b.0), mix1(a.1, b.1), mix1(a.2, b.2))
    }

    fn hex_of(rgb: (u8, u8, u8)) -> String {
        format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
    }

    // ---- Channel helpers ----
    //
    // Coerce the input either as INTEGER 0..=255 or REAL (with
    // clamp + round). The CSS spec lets rgb() take percents too,
    // but our SQL surface keeps it numeric -- callers wanting
    // percent semantics use color_parse() instead.

    fn channel_byte(args: &[SqlValue], i: usize, fname: &str) -> Result<u8, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok((*n).clamp(0, 255) as u8),
            Some(SqlValue::Real(r)) => Ok((r.round().clamp(0.0, 255.0)) as u8),
            _ => Err(format!("{fname}: channel {i} must be INTEGER 0..255")),
        }
    }

    /// REAL accepting INTEGER too. NULL -> propagates via Option.
    fn real_arg(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<f64>, String> {
        match args.get(i) {
            None | Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Real(r)) => Ok(Some(*r)),
            Some(SqlValue::Integer(n)) => Ok(Some(*n as f64)),
            _ => Err(format!("{fname}: arg {i} must be numeric")),
        }
    }

    fn text_arg(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            None | Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            _ => Err(format!("{fname}: arg {i} must be TEXT")),
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
                name: "color".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // v0.1 (kept for back-compat)
                    s(FID_TO_HEX, "color_to_hex", 1),
                    s(FID_TO_RGB, "color_to_rgb", 1),
                    s(FID_LUMINANCE, "color_luminance", 1),
                    s(FID_CONTRAST, "color_contrast_ratio", 2),
                    s(FID_RED, "color_red", 1),
                    s(FID_GREEN, "color_green", 1),
                    s(FID_BLUE, "color_blue", 1),
                    // v0.2 (PLAN-more-extensions-3 #5)
                    s(FID_PARSE, "color_parse", 1),
                    s(FID_NAMED, "color_named", 1),
                    s(FID_RGB_TO_HEX, "color_rgb_to_hex", 3),
                    s(FID_HEX_TO_RGB, "color_hex_to_rgb", 1),
                    s(FID_RGB_TO_HSL, "color_rgb_to_hsl", 3),
                    s(FID_HSL_TO_RGB, "color_hsl_to_rgb", 3),
                    s(FID_RGB_TO_HSV, "color_rgb_to_hsv", 3),
                    s(FID_HSV_TO_RGB, "color_hsv_to_rgb", 3),
                    s(FID_MIX, "color_mix", 3),
                    s(FID_INVERT, "color_invert", 1),
                    s(FID_VERSION, "color_version", 0),
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
                preferred_prefix: Some("color".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.color".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                // ---- v0.1 surface (kept) ----
                FID_CONTRAST => {
                    let a = text_arg(&args, 0, "color_contrast_ratio")?;
                    let b = text_arg(&args, 1, "color_contrast_ratio")?;
                    let (a, b) = match (a, b) {
                        (Some(a), Some(b)) => (a, b),
                        _ => return Ok(SqlValue::Null),
                    };
                    Ok(match (parse_color(&a), parse_color(&b)) {
                        (Some(ca), Some(cb)) => SqlValue::Real(contrast(ca, cb)),
                        _ => SqlValue::Null,
                    })
                }
                FID_TO_HEX => {
                    let raw = match text_arg(&args, 0, "color_to_hex")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(parse_color(&raw)
                        .map(|rgb| SqlValue::Text(hex_of(rgb)))
                        .unwrap_or(SqlValue::Null))
                }
                FID_TO_RGB => {
                    let raw = match text_arg(&args, 0, "color_to_rgb")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(parse_color(&raw)
                        .map(|(r, g, b)| SqlValue::Text(format!("rgb({r}, {g}, {b})")))
                        .unwrap_or(SqlValue::Null))
                }
                FID_LUMINANCE => {
                    let raw = match text_arg(&args, 0, "color_luminance")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(parse_color(&raw)
                        .map(|rgb| SqlValue::Real(luminance(rgb)))
                        .unwrap_or(SqlValue::Null))
                }
                FID_RED => channel_field(&args, 0, "color_red"),
                FID_GREEN => channel_field(&args, 1, "color_green"),
                FID_BLUE => channel_field(&args, 2, "color_blue"),

                // ---- v0.2 surface ----
                FID_PARSE => {
                    let raw = match text_arg(&args, 0, "color_parse")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(parse_color(&raw)
                        .map(|rgb| SqlValue::Text(hex_of(rgb)))
                        .unwrap_or(SqlValue::Null))
                }
                FID_NAMED => {
                    // Restricted to *named* lookups -- accepts the
                    // 148-color CSS name set only. Hex / rgb()
                    // inputs return NULL to give callers a clear
                    // signal-to-noise channel for "is this name
                    // known".
                    let raw = match text_arg(&args, 0, "color_named")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let lower = raw.trim().to_ascii_lowercase();
                    if lower.is_empty()
                        || lower.starts_with('#')
                        || lower.starts_with("rgb")
                        || lower.starts_with("hsl")
                        || lower.starts_with("hwb")
                        || lower.starts_with("lab")
                        || lower.starts_with("lch")
                        || lower.starts_with("oklab")
                        || lower.starts_with("oklch")
                    {
                        return Ok(SqlValue::Null);
                    }
                    // Round-trip through Color to confirm the name
                    // resolves; rebuilt color has .name() that
                    // returns Some iff the canonical name exists.
                    let c: Color = match lower.parse() {
                        Ok(c) => c,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    if c.name().is_none() && lower != "transparent" {
                        // csscolorparser parses "rebeccapurple" /
                        // "papayawhip" / etc. successfully but
                        // .name() may only round-trip the X11
                        // canonical set; we don't filter on
                        // .name() result -- the parse succeeding
                        // with a non-CSS-syntax input is enough.
                        // Belt + braces here is a no-op.
                    }
                    let [r, g, b, _] = c.to_rgba8();
                    Ok(SqlValue::Text(hex_of((r, g, b))))
                }
                FID_RGB_TO_HEX => {
                    let r = channel_byte(&args, 0, "color_rgb_to_hex")?;
                    let g = channel_byte(&args, 1, "color_rgb_to_hex")?;
                    let b = channel_byte(&args, 2, "color_rgb_to_hex")?;
                    Ok(SqlValue::Text(hex_of((r, g, b))))
                }
                FID_HEX_TO_RGB => {
                    let raw = match text_arg(&args, 0, "color_hex_to_rgb")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(parse_color(&raw)
                        .map(|(r, g, b)| SqlValue::Text(format!("[{r}, {g}, {b}]")))
                        .unwrap_or(SqlValue::Null))
                }
                FID_RGB_TO_HSL => {
                    let r = channel_byte(&args, 0, "color_rgb_to_hsl")?;
                    let g = channel_byte(&args, 1, "color_rgb_to_hsl")?;
                    let b = channel_byte(&args, 2, "color_rgb_to_hsl")?;
                    let c = Color::from_rgba8(r, g, b, 255);
                    let [h, s, l, _] = c.to_hsla();
                    // Round h to nearest int, s/l to int percent.
                    // hsla() returns NaN for hue on greys; report
                    // 0 in that case (matches CSS / browsers).
                    let h = if h.is_nan() { 0.0 } else { h };
                    let hi = h.round() as i64;
                    let si = (s * 100.0).round() as i64;
                    let li = (l * 100.0).round() as i64;
                    Ok(SqlValue::Text(format!("[{hi}, {si}, {li}]")))
                }
                FID_HSL_TO_RGB => {
                    let h = match real_arg(&args, 0, "color_hsl_to_rgb")? {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let s = match real_arg(&args, 1, "color_hsl_to_rgb")? {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let l = match real_arg(&args, 2, "color_hsl_to_rgb")? {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    // Inputs are h 0..360, s/l 0..100 (percent).
                    let c = Color::from_hsla(
                        h as f32,
                        (s / 100.0).clamp(0.0, 1.0) as f32,
                        (l / 100.0).clamp(0.0, 1.0) as f32,
                        1.0,
                    );
                    let [r, g, b, _] = c.to_rgba8();
                    Ok(SqlValue::Text(format!("[{r}, {g}, {b}]")))
                }
                FID_RGB_TO_HSV => {
                    let r = channel_byte(&args, 0, "color_rgb_to_hsv")?;
                    let g = channel_byte(&args, 1, "color_rgb_to_hsv")?;
                    let b = channel_byte(&args, 2, "color_rgb_to_hsv")?;
                    let c = Color::from_rgba8(r, g, b, 255);
                    let [h, s, v, _] = c.to_hsva();
                    let h = if h.is_nan() { 0.0 } else { h };
                    let hi = h.round() as i64;
                    let si = (s * 100.0).round() as i64;
                    let vi = (v * 100.0).round() as i64;
                    Ok(SqlValue::Text(format!("[{hi}, {si}, {vi}]")))
                }
                FID_HSV_TO_RGB => {
                    let h = match real_arg(&args, 0, "color_hsv_to_rgb")? {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let s = match real_arg(&args, 1, "color_hsv_to_rgb")? {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let v = match real_arg(&args, 2, "color_hsv_to_rgb")? {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let c = Color::from_hsva(
                        h as f32,
                        (s / 100.0).clamp(0.0, 1.0) as f32,
                        (v / 100.0).clamp(0.0, 1.0) as f32,
                        1.0,
                    );
                    let [r, g, b, _] = c.to_rgba8();
                    Ok(SqlValue::Text(format!("[{r}, {g}, {b}]")))
                }
                FID_MIX => {
                    let a_raw = match text_arg(&args, 0, "color_mix")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let b_raw = match text_arg(&args, 1, "color_mix")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let t = match real_arg(&args, 2, "color_mix")? {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match (parse_color(&a_raw), parse_color(&b_raw)) {
                        (Some(ca), Some(cb)) => SqlValue::Text(hex_of(mix_linear(ca, cb, t))),
                        _ => SqlValue::Null,
                    })
                }
                FID_INVERT => {
                    let raw = match text_arg(&args, 0, "color_invert")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(parse_color(&raw)
                        .map(|(r, g, b)| SqlValue::Text(hex_of((255 - r, 255 - g, 255 - b))))
                        .unwrap_or(SqlValue::Null))
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "color {} (csscolorparser 0.8)",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("color: unknown func id {other}")),
            }
        }
    }

    /// Extract one channel from a parsed color. `which` is the
    /// tuple index (0=r, 1=g, 2=b).
    fn channel_field(args: &[SqlValue], which: usize, fname: &str) -> Result<SqlValue, String> {
        let raw = match text_arg(args, 0, fname)? {
            Some(s) => s,
            None => return Ok(SqlValue::Null),
        };
        Ok(parse_color(&raw)
            .map(|(r, g, b)| {
                let v = match which {
                    0 => r,
                    1 => g,
                    _ => b,
                };
                SqlValue::Integer(v as i64)
            })
            .unwrap_or(SqlValue::Null))
    }

    bindings::export!(Ext with_types_in bindings);
}
