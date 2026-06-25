//! Embed path for semver. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::ffi::c_int;
use core::str::FromStr;
use semver::{Version, VersionReq};
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_MAJOR: u64 = 2;
const FID_MINOR: u64 = 3;
const FID_PATCH: u64 = 4;
const FID_PRE: u64 = 5;
const FID_BUILD: u64 = 6;
const FID_COMPARE: u64 = 7;
const FID_MAX: u64 = 8;
const FID_SATISFIES: u64 = 9;
const FID_INCREMENT: u64 = 10;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn parse_or_null(s: &str) -> Option<Version> {
    Version::parse(s).ok()
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VALIDATE => {
            let v = arg_text(&args, 0, "semver_validate")?;
            Ok(SqlValueOwned::Integer(parse_or_null(&v).is_some() as i64))
        }
        FID_MAJOR => Ok(parse_or_null(&arg_text(&args, 0, "semver_major")?)
            .map(|v| SqlValueOwned::Integer(v.major as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_MINOR => Ok(parse_or_null(&arg_text(&args, 0, "semver_minor")?)
            .map(|v| SqlValueOwned::Integer(v.minor as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_PATCH => Ok(parse_or_null(&arg_text(&args, 0, "semver_patch")?)
            .map(|v| SqlValueOwned::Integer(v.patch as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_PRE => Ok(parse_or_null(&arg_text(&args, 0, "semver_pre")?)
            .map(|v| {
                if v.pre.is_empty() {
                    SqlValueOwned::Null
                } else {
                    SqlValueOwned::Text(v.pre.to_string())
                }
            })
            .unwrap_or(SqlValueOwned::Null)),
        FID_BUILD => Ok(parse_or_null(&arg_text(&args, 0, "semver_build")?)
            .map(|v| {
                if v.build.is_empty() {
                    SqlValueOwned::Null
                } else {
                    SqlValueOwned::Text(v.build.to_string())
                }
            })
            .unwrap_or(SqlValueOwned::Null)),
        FID_COMPARE => {
            let a = arg_text(&args, 0, "semver_compare")?;
            let b = arg_text(&args, 1, "semver_compare")?;
            let va = Version::parse(&a).map_err(|e| format!("semver_compare a: {e}"))?;
            let vb = Version::parse(&b).map_err(|e| format!("semver_compare b: {e}"))?;
            Ok(SqlValueOwned::Integer(match va.cmp(&vb) {
                Ordering::Less => -1,
                Ordering::Equal => 0,
                Ordering::Greater => 1,
            }))
        }
        FID_MAX => {
            let a = arg_text(&args, 0, "semver_max")?;
            let b = arg_text(&args, 1, "semver_max")?;
            let va = parse_or_null(&a);
            let vb = parse_or_null(&b);
            Ok(match (va, vb) {
                (Some(va), Some(vb)) => {
                    if va >= vb {
                        SqlValueOwned::Text(a)
                    } else {
                        SqlValueOwned::Text(b)
                    }
                }
                (Some(_), None) => SqlValueOwned::Text(a),
                (None, Some(_)) => SqlValueOwned::Text(b),
                (None, None) => SqlValueOwned::Null,
            })
        }
        FID_SATISFIES => {
            let v = arg_text(&args, 0, "semver_satisfies")?;
            let req = arg_text(&args, 1, "semver_satisfies")?;
            let parsed_v = parse_or_null(&v);
            let parsed_req = VersionReq::from_str(&req).ok();
            Ok(match (parsed_v, parsed_req) {
                (Some(vv), Some(rr)) => SqlValueOwned::Integer(rr.matches(&vv) as i64),
                _ => SqlValueOwned::Null,
            })
        }
        FID_INCREMENT => {
            let v = arg_text(&args, 0, "semver_increment")?;
            let part = arg_text(&args, 1, "semver_increment")?;
            let mut parsed = match parse_or_null(&v) {
                Some(p) => p,
                None => return Ok(SqlValueOwned::Null),
            };
            match part.to_ascii_lowercase().as_str() {
                "major" => {
                    parsed.major += 1;
                    parsed.minor = 0;
                    parsed.patch = 0;
                }
                "minor" => {
                    parsed.minor += 1;
                    parsed.patch = 0;
                }
                "patch" => {
                    parsed.patch += 1;
                }
                other => return Err(format!("semver_increment: bad part {other:?}")),
            }
            parsed.pre = semver::Prerelease::EMPTY;
            parsed.build = semver::BuildMetadata::EMPTY;
            Ok(SqlValueOwned::Text(parsed.to_string()))
        }
        other => Err(format!("semver: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"semver_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MAJOR,
        name: b"semver_major\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MINOR,
        name: b"semver_minor\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PATCH,
        name: b"semver_patch\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PRE,
        name: b"semver_pre\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BUILD,
        name: b"semver_build\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_COMPARE,
        name: b"semver_compare\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MAX,
        name: b"semver_max\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SATISFIES,
        name: b"semver_satisfies\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_INCREMENT,
        name: b"semver_increment\0",
        num_args: 2,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
