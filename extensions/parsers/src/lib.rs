//! Color + unit + financial-ID parsers.

extern crate alloc;

use alloc::string::{String, ToString};

// ── Color ──────────────────────────────────────────────────

pub fn hex_to_rgb(hex: &str) -> Result<(u8, u8, u8), String> {
    let s = hex.trim().trim_start_matches('#');
    if s.len() != 6 {
        return Err(alloc::format!("hex_to_rgb: expected 6 hex chars, got {s:?}"));
    }
    let r = u8::from_str_radix(&s[0..2], 16).map_err(|e| alloc::format!("hex_to_rgb: {e}"))?;
    let g = u8::from_str_radix(&s[2..4], 16).map_err(|e| alloc::format!("hex_to_rgb: {e}"))?;
    let b = u8::from_str_radix(&s[4..6], 16).map_err(|e| alloc::format!("hex_to_rgb: {e}"))?;
    Ok((r, g, b))
}

pub fn rgb_to_hex(r: u8, g: u8, b: u8) -> String {
    alloc::format!("#{:02x}{:02x}{:02x}", r, g, b)
}

pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let rf = r as f64 / 255.0;
    let gf = g as f64 / 255.0;
    let bf = b as f64 / 255.0;
    let max = rf.max(gf.max(bf));
    let min = rf.min(gf.min(bf));
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-9 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if (max - rf).abs() < 1e-9 {
        (gf - bf) / d + if gf < bf { 6.0 } else { 0.0 }
    } else if (max - gf).abs() < 1e-9 {
        (bf - rf) / d + 2.0
    } else {
        (rf - gf) / d + 4.0
    };
    (h * 60.0, s, l)
}

pub fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 1.0 / 2.0 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        }
        p
    }
    if s < 1e-9 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let h = h / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);
    (
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

pub fn color_lighten(hex: &str, pct: f64) -> Result<String, String> {
    let (r, g, b) = hex_to_rgb(hex)?;
    let (h, s, l) = rgb_to_hsl(r, g, b);
    let l = (l + pct / 100.0).clamp(0.0, 1.0);
    let (r2, g2, b2) = hsl_to_rgb(h, s, l);
    Ok(rgb_to_hex(r2, g2, b2))
}

// ── Unit conversion ────────────────────────────────────────

/// Length to meters.
fn length_to_meters(unit: &str) -> Option<f64> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "mm" => 0.001,
        "cm" => 0.01,
        "m" | "meter" | "meters" => 1.0,
        "km" => 1000.0,
        "in" | "inch" | "inches" => 0.0254,
        "ft" | "foot" | "feet" => 0.3048,
        "yd" | "yard" | "yards" => 0.9144,
        "mi" | "mile" | "miles" => 1609.344,
        "nmi" | "nautical-mile" => 1852.0,
        _ => return None,
    })
}

pub fn convert_length(value: f64, from: &str, to: &str) -> Result<f64, String> {
    let f = length_to_meters(from)
        .ok_or_else(|| alloc::format!("convert_length: unknown unit {from:?}"))?;
    let t = length_to_meters(to)
        .ok_or_else(|| alloc::format!("convert_length: unknown unit {to:?}"))?;
    Ok(value * f / t)
}

fn mass_to_grams(unit: &str) -> Option<f64> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "mg" => 0.001,
        "g" | "gram" | "grams" => 1.0,
        "kg" => 1000.0,
        "oz" | "ounce" | "ounces" => 28.3495231,
        "lb" | "pound" | "pounds" => 453.59237,
        "st" | "stone" => 6350.29318,
        "t" | "ton" | "metric-ton" => 1_000_000.0,
        _ => return None,
    })
}

pub fn convert_mass(value: f64, from: &str, to: &str) -> Result<f64, String> {
    let f = mass_to_grams(from)
        .ok_or_else(|| alloc::format!("convert_mass: unknown unit {from:?}"))?;
    let t = mass_to_grams(to)
        .ok_or_else(|| alloc::format!("convert_mass: unknown unit {to:?}"))?;
    Ok(value * f / t)
}

pub fn convert_temperature(value: f64, from: &str, to: &str) -> Result<f64, String> {
    let kelvin = match from.to_ascii_lowercase().as_str() {
        "c" | "celsius" => value + 273.15,
        "f" | "fahrenheit" => (value - 32.0) * 5.0 / 9.0 + 273.15,
        "k" | "kelvin" => value,
        other => return Err(alloc::format!("convert_temperature: unknown {other:?}")),
    };
    Ok(match to.to_ascii_lowercase().as_str() {
        "c" | "celsius" => kelvin - 273.15,
        "f" | "fahrenheit" => (kelvin - 273.15) * 9.0 / 5.0 + 32.0,
        "k" | "kelvin" => kelvin,
        other => return Err(alloc::format!("convert_temperature: unknown {other:?}")),
    })
}

// ── Luhn ───────────────────────────────────────────────────

pub fn luhn_check(s: &str) -> bool {
    let digits: alloc::vec::Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.is_empty() {
        return false;
    }
    let n = digits.len();
    let mut sum = 0u32;
    for (i, d) in digits.iter().enumerate() {
        let pos_from_right = n - 1 - i;
        if pos_from_right % 2 == 1 {
            let doubled = d * 2;
            sum += if doubled > 9 { doubled - 9 } else { doubled };
        } else {
            sum += d;
        }
    }
    sum % 10 == 0
}

// ── IBAN ───────────────────────────────────────────────────

pub fn iban_validate(iban: &str) -> bool {
    let s: String = iban.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() < 4 || s.len() > 34 {
        return false;
    }
    // Move first 4 chars (country + check) to the end.
    let rotated: String = s[4..].chars().chain(s[..4].chars()).collect();
    // Replace letters with two-digit codes (A=10, B=11, ...,
    // Z=35), then compute mod 97.
    let mut numeric = String::with_capacity(rotated.len() * 2);
    for c in rotated.chars() {
        match c {
            '0'..='9' => numeric.push(c),
            'a'..='z' | 'A'..='Z' => {
                let v = c.to_ascii_uppercase() as u32 - b'A' as u32 + 10;
                numeric.push_str(&alloc::format!("{v}"));
            }
            _ => return false,
        }
    }
    // Compute mod 97 over the big string by streaming.
    let mut rem: u64 = 0;
    for c in numeric.chars() {
        let d = c.to_digit(10).unwrap() as u64;
        rem = (rem * 10 + d) % 97;
    }
    rem == 1
}

pub fn iban_format(iban: &str) -> String {
    let s: String = iban.chars().filter(|c| !c.is_whitespace()).map(|c| c.to_ascii_uppercase()).collect();
    let mut out = String::with_capacity(s.len() + s.len() / 4);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_rgb_round_trip() {
        let (r, g, b) = hex_to_rgb("#ff8000").unwrap();
        assert_eq!((r, g, b), (255, 128, 0));
        assert_eq!(rgb_to_hex(r, g, b), "#ff8000");
    }

    #[test]
    fn rgb_hsl_round_trip() {
        let (h, s, l) = rgb_to_hsl(255, 0, 0);
        assert!(h.abs() < 1.0);
        assert!((s - 1.0).abs() < 1e-6);
        assert!((l - 0.5).abs() < 1e-6);
        let (r, g, b) = hsl_to_rgb(h, s, l);
        assert_eq!((r, g, b), (255, 0, 0));
    }

    #[test]
    fn lighten_increases_l() {
        let lit = color_lighten("#404040", 20.0).unwrap();
        // Original l = 0.25; +0.20  0.45  ~#737373.
        let (r, _, _) = hex_to_rgb(&lit).unwrap();
        assert!(r > 0x40);
    }

    #[test]
    fn convert_length_km_to_mi() {
        // 1 km ~ 0.6213712 mi.
        let v = convert_length(1.0, "km", "mi").unwrap();
        assert!((v - 0.6213712).abs() < 1e-5);
    }

    #[test]
    fn convert_temperature_c_to_f() {
        assert!((convert_temperature(100.0, "c", "f").unwrap() - 212.0).abs() < 1e-6);
        assert!((convert_temperature(0.0, "c", "f").unwrap() - 32.0).abs() < 1e-6);
    }

    #[test]
    fn luhn_known_good_and_bad() {
        // Common Visa test number.
        assert!(luhn_check("4111 1111 1111 1111"));
        assert!(!luhn_check("4111 1111 1111 1112"));
        assert!(!luhn_check(""));
    }

    #[test]
    fn iban_known_valid() {
        // Wikipedia test IBANs.
        assert!(iban_validate("GB82 WEST 1234 5698 7654 32"));
        assert!(iban_validate("DE89 3704 0044 0532 0130 00"));
        // Off by one in the check digits  fails.
        assert!(!iban_validate("GB83 WEST 1234 5698 7654 32"));
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

    const FID_HEX_RGB: u64 = 1;
    const FID_RGB_HEX: u64 = 2;
    const FID_RGB_HSL: u64 = 3;
    const FID_HSL_RGB: u64 = 4;
    const FID_LIGHTEN: u64 = 5;
    const FID_DARKEN: u64 = 6;
    const FID_LEN: u64 = 10;
    const FID_MASS: u64 = 11;
    const FID_TEMP: u64 = 12;
    const FID_LUHN: u64 = 20;
    const FID_IBAN_V: u64 = 21;
    const FID_IBAN_F: u64 = 22;

    struct Ext;

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
                name: "parsers".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_HEX_RGB, "hex_to_rgb", 1),
                    s(FID_RGB_HEX, "rgb_to_hex", 3),
                    s(FID_RGB_HSL, "rgb_to_hsl", 3),
                    s(FID_HSL_RGB, "hsl_to_rgb", 3),
                    s(FID_LIGHTEN, "color_lighten", 2),
                    s(FID_DARKEN, "color_darken", 2),
                    s(FID_LEN, "convert_length", 3),
                    s(FID_MASS, "convert_mass", 3),
                    s(FID_TEMP, "convert_temperature", 3),
                    s(FID_LUHN, "luhn_check", 1),
                    s(FID_IBAN_V, "iban_validate", 1),
                    s(FID_IBAN_F, "iban_format", 1),
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

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_HEX_RGB => {
                    let h = arg_text(&args, 0, "hex_to_rgb")?;
                    let (r, g, b) = super::hex_to_rgb(&h)?;
                    Ok(SqlValue::Text(format!("{r},{g},{b}")))
                }
                FID_RGB_HEX => {
                    let r = arg_int(&args, 0, "rgb_to_hex")? as u8;
                    let g = arg_int(&args, 1, "rgb_to_hex")? as u8;
                    let b = arg_int(&args, 2, "rgb_to_hex")? as u8;
                    Ok(SqlValue::Text(super::rgb_to_hex(r, g, b)))
                }
                FID_RGB_HSL => {
                    let r = arg_int(&args, 0, "rgb_to_hsl")? as u8;
                    let g = arg_int(&args, 1, "rgb_to_hsl")? as u8;
                    let b = arg_int(&args, 2, "rgb_to_hsl")? as u8;
                    let (h, s, l) = super::rgb_to_hsl(r, g, b);
                    Ok(SqlValue::Text(format!("{h},{s},{l}")))
                }
                FID_HSL_RGB => {
                    let h = arg_real(&args, 0, "hsl_to_rgb")?;
                    let s = arg_real(&args, 1, "hsl_to_rgb")?;
                    let l = arg_real(&args, 2, "hsl_to_rgb")?;
                    let (r, g, b) = super::hsl_to_rgb(h, s, l);
                    Ok(SqlValue::Text(format!("{r},{g},{b}")))
                }
                FID_LIGHTEN => {
                    let h = arg_text(&args, 0, "color_lighten")?;
                    let p = arg_real(&args, 1, "color_lighten")?;
                    super::color_lighten(&h, p).map(SqlValue::Text)
                }
                FID_DARKEN => {
                    let h = arg_text(&args, 0, "color_darken")?;
                    let p = arg_real(&args, 1, "color_darken")?;
                    super::color_lighten(&h, -p).map(SqlValue::Text)
                }
                FID_LEN => {
                    let v = arg_real(&args, 0, "convert_length")?;
                    let f = arg_text(&args, 1, "convert_length")?;
                    let t = arg_text(&args, 2, "convert_length")?;
                    super::convert_length(v, &f, &t).map(SqlValue::Real)
                }
                FID_MASS => {
                    let v = arg_real(&args, 0, "convert_mass")?;
                    let f = arg_text(&args, 1, "convert_mass")?;
                    let t = arg_text(&args, 2, "convert_mass")?;
                    super::convert_mass(v, &f, &t).map(SqlValue::Real)
                }
                FID_TEMP => {
                    let v = arg_real(&args, 0, "convert_temperature")?;
                    let f = arg_text(&args, 1, "convert_temperature")?;
                    let t = arg_text(&args, 2, "convert_temperature")?;
                    super::convert_temperature(v, &f, &t).map(SqlValue::Real)
                }
                FID_LUHN => Ok(SqlValue::Integer(
                    super::luhn_check(&arg_text(&args, 0, "luhn_check")?) as i64,
                )),
                FID_IBAN_V => Ok(SqlValue::Integer(
                    super::iban_validate(&arg_text(&args, 0, "iban_validate")?) as i64,
                )),
                FID_IBAN_F => Ok(SqlValue::Text(super::iban_format(&arg_text(
                    &args,
                    0,
                    "iban_format",
                )?))),
                other => Err(format!("parsers: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
