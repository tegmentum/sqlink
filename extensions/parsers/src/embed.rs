//! Embed path for parsers. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

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

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_real(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<f64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Real(r)) => Ok(*r),
        Some(SqlValueOwned::Integer(n)) => Ok(*n as f64),
        _ => Err(format!("{fname}: numeric arg at {i}")),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(r)) => Ok(*r as i64),
        _ => Err(format!("{fname}: integer arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_HEX_RGB => {
            let h = arg_text(&args, 0, "hex_to_rgb")?;
            let (r, g, b) = crate::hex_to_rgb(&h)?;
            Ok(SqlValueOwned::Text(format!("{r},{g},{b}")))
        }
        FID_RGB_HEX => {
            let r = arg_int(&args, 0, "rgb_to_hex")? as u8;
            let g = arg_int(&args, 1, "rgb_to_hex")? as u8;
            let b = arg_int(&args, 2, "rgb_to_hex")? as u8;
            Ok(SqlValueOwned::Text(crate::rgb_to_hex(r, g, b)))
        }
        FID_RGB_HSL => {
            let r = arg_int(&args, 0, "rgb_to_hsl")? as u8;
            let g = arg_int(&args, 1, "rgb_to_hsl")? as u8;
            let b = arg_int(&args, 2, "rgb_to_hsl")? as u8;
            let (h, s, l) = crate::rgb_to_hsl(r, g, b);
            Ok(SqlValueOwned::Text(format!("{h},{s},{l}")))
        }
        FID_HSL_RGB => {
            let h = arg_real(&args, 0, "hsl_to_rgb")?;
            let s = arg_real(&args, 1, "hsl_to_rgb")?;
            let l = arg_real(&args, 2, "hsl_to_rgb")?;
            let (r, g, b) = crate::hsl_to_rgb(h, s, l);
            Ok(SqlValueOwned::Text(format!("{r},{g},{b}")))
        }
        FID_LIGHTEN => {
            let h = arg_text(&args, 0, "color_lighten")?;
            let p = arg_real(&args, 1, "color_lighten")?;
            crate::color_lighten(&h, p).map(SqlValueOwned::Text)
        }
        FID_DARKEN => {
            let h = arg_text(&args, 0, "color_darken")?;
            let p = arg_real(&args, 1, "color_darken")?;
            crate::color_lighten(&h, -p).map(SqlValueOwned::Text)
        }
        FID_LEN => {
            let v = arg_real(&args, 0, "convert_length")?;
            let f = arg_text(&args, 1, "convert_length")?;
            let t = arg_text(&args, 2, "convert_length")?;
            crate::convert_length(v, &f, &t).map(SqlValueOwned::Real)
        }
        FID_MASS => {
            let v = arg_real(&args, 0, "convert_mass")?;
            let f = arg_text(&args, 1, "convert_mass")?;
            let t = arg_text(&args, 2, "convert_mass")?;
            crate::convert_mass(v, &f, &t).map(SqlValueOwned::Real)
        }
        FID_TEMP => {
            let v = arg_real(&args, 0, "convert_temperature")?;
            let f = arg_text(&args, 1, "convert_temperature")?;
            let t = arg_text(&args, 2, "convert_temperature")?;
            crate::convert_temperature(v, &f, &t).map(SqlValueOwned::Real)
        }
        FID_LUHN => Ok(SqlValueOwned::Integer(
            crate::luhn_check(&arg_text(&args, 0, "luhn_check")?) as i64,
        )),
        FID_IBAN_V => Ok(SqlValueOwned::Integer(
            crate::iban_validate(&arg_text(&args, 0, "iban_validate")?) as i64,
        )),
        FID_IBAN_F => Ok(SqlValueOwned::Text(crate::iban_format(&arg_text(
            &args,
            0,
            "iban_format",
        )?))),
        other => Err(format!("parsers: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_HEX_RGB, name: b"hex_to_rgb\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_RGB_HEX, name: b"rgb_to_hex\0",          num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_RGB_HSL, name: b"rgb_to_hsl\0",          num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_HSL_RGB, name: b"hsl_to_rgb\0",          num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_LIGHTEN, name: b"color_lighten\0",       num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_DARKEN,  name: b"color_darken\0",        num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_LEN,     name: b"convert_length\0",      num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_MASS,    name: b"convert_mass\0",        num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_TEMP,    name: b"convert_temperature\0", num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_LUHN,    name: b"luhn_check\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_IBAN_V,  name: b"iban_validate\0",       num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_IBAN_F,  name: b"iban_format\0",         num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
