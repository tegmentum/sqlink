//! Embed path for emoji. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};
use unicode_segmentation::UnicodeSegmentation;

const FID_COUNT: u64 = 1;
const FID_EXTRACT: u64 = 2;
const FID_STRIP: u64 = 3;
const FID_FROM_SHORTCODE: u64 = 4;
const FID_SHORTCODE: u64 = 5;
const FID_NAME: u64 = 6;
const FID_GROUP: u64 = 7;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn each_emoji_grapheme<F>(text: &str, mut f: F)
where
    F: FnMut(&str),
{
    for g in text.graphemes(true) {
        if emojis::get(g).is_some() {
            f(g);
        }
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_COUNT => {
            let t = arg_text(&args, 0, "emoji_count")?;
            let mut n = 0i64;
            each_emoji_grapheme(&t, |_| n += 1);
            Ok(SqlValueOwned::Integer(n))
        }
        FID_EXTRACT => {
            let t = arg_text(&args, 0, "emoji_extract")?;
            let mut out: Vec<String> = Vec::new();
            each_emoji_grapheme(&t, |g| out.push(g.to_string()));
            Ok(SqlValueOwned::Text(
                serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string()),
            ))
        }
        FID_STRIP => {
            let t = arg_text(&args, 0, "emoji_strip")?;
            let kept: String = t
                .graphemes(true)
                .filter(|g| emojis::get(g).is_none())
                .collect();
            Ok(SqlValueOwned::Text(kept))
        }
        FID_FROM_SHORTCODE => {
            let sc = arg_text(&args, 0, "emoji_from_shortcode")?;
            // accept both ":sparkles:" and "sparkles"
            let trimmed = sc.trim_matches(':');
            match emojis::get_by_shortcode(trimmed) {
                Some(e) => Ok(SqlValueOwned::Text(e.as_str().to_string())),
                None => Ok(SqlValueOwned::Null),
            }
        }
        FID_SHORTCODE => {
            let t = arg_text(&args, 0, "emoji_shortcode")?;
            match emojis::get(&t).and_then(|e| e.shortcode()) {
                Some(sc) => Ok(SqlValueOwned::Text(sc.to_string())),
                None => Ok(SqlValueOwned::Null),
            }
        }
        FID_NAME => {
            let t = arg_text(&args, 0, "emoji_name")?;
            match emojis::get(&t) {
                Some(e) => Ok(SqlValueOwned::Text(e.name().to_string())),
                None => Ok(SqlValueOwned::Null),
            }
        }
        FID_GROUP => {
            let t = arg_text(&args, 0, "emoji_group")?;
            match emojis::get(&t) {
                Some(e) => Ok(SqlValueOwned::Text(format!("{:?}", e.group()).to_lowercase())),
                None => Ok(SqlValueOwned::Null),
            }
        }
        other => Err(format!("emoji: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_COUNT,          name: b"emoji_count\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_EXTRACT,        name: b"emoji_extract\0",        num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_STRIP,          name: b"emoji_strip\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_FROM_SHORTCODE, name: b"emoji_from_shortcode\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SHORTCODE,      name: b"emoji_shortcode\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NAME,           name: b"emoji_name\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_GROUP,          name: b"emoji_group\0",          num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
