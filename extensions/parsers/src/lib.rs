//! Small parsers and converters.
//!
//! Duplicates of focused extensions have been removed:
//!   * hex_to_rgb / hsl_to_rgb / color_lighten / color_darken
//!     are now in extensions/color.
//!   * convert_length / convert_mass / convert_temperature are
//!     now in extensions/unitconv.
//!   * iban_validate / iban_format are now in extensions/iban.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::{String, ToString};

// ── Color (helpers retained for the two functions below) ───

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_to_hex_basic() {
        assert_eq!(rgb_to_hex(255, 128, 0), "#ff8000");
        assert_eq!(rgb_to_hex(0, 0, 0), "#000000");
    }

    #[test]
    fn rgb_to_hsl_basic() {
        let (h, s, l) = rgb_to_hsl(255, 0, 0);
        assert!(h.abs() < 1.0);
        assert!((s - 1.0).abs() < 1e-6);
        assert!((l - 0.5).abs() < 1e-6);
    }

    #[test]
    fn luhn_known_good_and_bad() {
        // Common Visa test number.
        assert!(luhn_check("4111 1111 1111 1111"));
        assert!(!luhn_check("4111 1111 1111 1112"));
        assert!(!luhn_check(""));
    }
}

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

    const FID_RGB_HEX: u64 = 2;
    const FID_RGB_HSL: u64 = 3;
    const FID_LUHN: u64 = 20;

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
                    s(FID_RGB_HEX, "rgb_to_hex", 3),
                    s(FID_RGB_HSL, "rgb_to_hsl", 3),
                    s(FID_LUHN, "luhn_check", 1),
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
                FID_LUHN => Ok(SqlValue::Integer(
                    super::luhn_check(&arg_text(&args, 0, "luhn_check")?) as i64,
                )),
                other => Err(format!("parsers: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
