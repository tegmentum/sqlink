//! Embed path for color. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_TO_HEX: u64 = 1;
const FID_TO_RGB: u64 = 2;
const FID_LUMINANCE: u64 = 3;
const FID_CONTRAST: u64 = 4;
const FID_RED: u64 = 5;
const FID_GREEN: u64 = 6;
const FID_BLUE: u64 = 7;

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
    let inner = s
        .strip_prefix("rgba(")
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

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    if func_id == FID_CONTRAST {
        let a = arg_text(&args, 0, "color_contrast_ratio")?;
        let b = arg_text(&args, 1, "color_contrast_ratio")?;
        return Ok(match (parse(&a), parse(&b)) {
            (Some(ca), Some(cb)) => {
                let la = luminance(ca);
                let lb = luminance(cb);
                let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
                SqlValueOwned::Real((hi + 0.05) / (lo + 0.05))
            }
            _ => SqlValueOwned::Null,
        });
    }

    let raw = arg_text(&args, 0, "color")?;
    let parsed = parse(&raw);
    match func_id {
        FID_TO_HEX => Ok(parsed
            .map(|(r, g, b)| SqlValueOwned::Text(format!("#{r:02x}{g:02x}{b:02x}")))
            .unwrap_or(SqlValueOwned::Null)),
        FID_TO_RGB => Ok(parsed
            .map(|(r, g, b)| SqlValueOwned::Text(format!("rgb({r}, {g}, {b})")))
            .unwrap_or(SqlValueOwned::Null)),
        FID_LUMINANCE => Ok(parsed
            .map(|rgb| SqlValueOwned::Real(luminance(rgb)))
            .unwrap_or(SqlValueOwned::Null)),
        FID_RED => Ok(parsed
            .map(|(r, _, _)| SqlValueOwned::Integer(r as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_GREEN => Ok(parsed
            .map(|(_, g, _)| SqlValueOwned::Integer(g as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_BLUE => Ok(parsed
            .map(|(_, _, b)| SqlValueOwned::Integer(b as i64))
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("color: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_TO_HEX,    name: b"color_to_hex\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TO_RGB,    name: b"color_to_rgb\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_LUMINANCE, name: b"color_luminance\0",       num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CONTRAST,  name: b"color_contrast_ratio\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_RED,       name: b"color_red\0",             num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_GREEN,     name: b"color_green\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_BLUE,      name: b"color_blue\0",            num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
