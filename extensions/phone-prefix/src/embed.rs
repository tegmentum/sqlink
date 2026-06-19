//! Embed path for phone-prefix. The lookup table is duplicated here
//! since the WIT-path version lives inside `mod wasm_export`; pushed
//! out as future cleanup (hoist into `pub mod data`).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_COUNTRY:   u64 = 1;
const FID_REGION:    u64 = 2;
const FID_NORMALIZE: u64 = 3;
const FID_PREFIX:    u64 = 4;

fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '+' || c.is_ascii_digit() {
            out.push(c);
        }
    }
    out
}

const TABLE: &[(&str, &str, &str)] = &[
    ("1242","BS","North America"),("1246","BB","North America"),
    ("1264","AI","North America"),("1268","AG","North America"),
    ("1284","VG","North America"),("1340","VI","North America"),
    ("1345","KY","North America"),("1441","BM","North America"),
    ("1473","GD","North America"),("1649","TC","North America"),
    ("1664","MS","North America"),("1670","MP","North America"),
    ("1671","GU","North America"),("1684","AS","North America"),
    ("1721","SX","North America"),("1758","LC","North America"),
    ("1767","DM","North America"),("1784","VC","North America"),
    ("1787","PR","North America"),("1809","DO","North America"),
    ("1868","TT","North America"),("1869","KN","North America"),
    ("1876","JM","North America"),
    ("254","KE","Africa"),("351","PT","Europe"),("352","LU","Europe"),
    ("353","IE","Europe"),("354","IS","Europe"),("355","AL","Europe"),
    ("356","MT","Europe"),("357","CY","Europe"),("358","FI","Europe"),
    ("359","BG","Europe"),("370","LT","Europe"),("371","LV","Europe"),
    ("372","EE","Europe"),("373","MD","Europe"),("374","AM","Asia"),
    ("375","BY","Europe"),("376","AD","Europe"),("377","MC","Europe"),
    ("378","SM","Europe"),("380","UA","Europe"),("381","RS","Europe"),
    ("385","HR","Europe"),("386","SI","Europe"),("387","BA","Europe"),
    ("389","MK","Europe"),("420","CZ","Europe"),("421","SK","Europe"),
    ("852","HK","Asia"),("853","MO","Asia"),("886","TW","Asia"),
    ("960","MV","Asia"),("961","LB","Asia"),("962","JO","Asia"),
    ("963","SY","Asia"),("964","IQ","Asia"),("965","KW","Asia"),
    ("966","SA","Asia"),("967","YE","Asia"),("968","OM","Asia"),
    ("971","AE","Asia"),("972","IL","Asia"),("973","BH","Asia"),
    ("974","QA","Asia"),
    ("20","EG","Africa"),("27","ZA","Africa"),
    ("30","GR","Europe"),("31","NL","Europe"),("32","BE","Europe"),
    ("33","FR","Europe"),("34","ES","Europe"),("36","HU","Europe"),
    ("39","IT","Europe"),("40","RO","Europe"),("41","CH","Europe"),
    ("43","AT","Europe"),("44","GB","Europe"),("45","DK","Europe"),
    ("46","SE","Europe"),("47","NO","Europe"),("48","PL","Europe"),
    ("49","DE","Europe"),
    ("51","PE","South America"),("52","MX","North America"),
    ("53","CU","North America"),("54","AR","South America"),
    ("55","BR","South America"),("56","CL","South America"),
    ("57","CO","South America"),("58","VE","South America"),
    ("60","MY","Asia"),("61","AU","Oceania"),("62","ID","Asia"),
    ("63","PH","Asia"),("64","NZ","Oceania"),("65","SG","Asia"),
    ("66","TH","Asia"),("81","JP","Asia"),("82","KR","Asia"),
    ("84","VN","Asia"),("86","CN","Asia"),("90","TR","Asia"),
    ("91","IN","Asia"),("92","PK","Asia"),("93","AF","Asia"),
    ("94","LK","Asia"),("95","MM","Asia"),("98","IR","Asia"),
    ("1","US","North America"),("7","RU","Europe"),
];

fn lookup(raw: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    let n = normalize(raw);
    let digits = n.trim_start_matches('+');
    for entry in TABLE.iter() {
        if digits.starts_with(entry.0) {
            return Some(entry);
        }
    }
    None
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "phone_prefix")?;
    match func_id {
        FID_COUNTRY => Ok(lookup(&raw)
            .map(|(_, cc, _)| SqlValueOwned::Text(cc.to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_REGION => Ok(lookup(&raw)
            .map(|(_, _, r)| SqlValueOwned::Text(r.to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_NORMALIZE => Ok(SqlValueOwned::Text(normalize(&raw))),
        FID_PREFIX => Ok(lookup(&raw)
            .map(|(p, _, _)| SqlValueOwned::Text(p.to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("phone_prefix: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_COUNTRY,   name: b"phone_prefix_country\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_REGION,    name: b"phone_prefix_region\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NORMALIZE, name: b"phone_prefix_normalize\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_PREFIX,    name: b"phone_prefix_prefix\0",    num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
