//! Semantic Versioning scalars.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cmp::Ordering;
    use core::str::FromStr;

    use semver::{Version, VersionReq};

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

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse_or_null(s: &str) -> Option<Version> {
        Version::parse(s).ok()
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
                name: "semver".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "semver_validate", 1),
                    s(FID_MAJOR, "semver_major", 1),
                    s(FID_MINOR, "semver_minor", 1),
                    s(FID_PATCH, "semver_patch", 1),
                    s(FID_PRE, "semver_pre", 1),
                    s(FID_BUILD, "semver_build", 1),
                    s(FID_COMPARE, "semver_compare", 2),
                    s(FID_MAX, "semver_max", 2),
                    s(FID_SATISFIES, "semver_satisfies", 2),
                    s(FID_INCREMENT, "semver_increment", 2),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VALIDATE => {
                    let v = arg_text(&args, 0, "semver_validate")?;
                    Ok(SqlValue::Integer(parse_or_null(&v).is_some() as i64))
                }
                FID_MAJOR => Ok(parse_or_null(&arg_text(&args, 0, "semver_major")?)
                    .map(|v| SqlValue::Integer(v.major as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_MINOR => Ok(parse_or_null(&arg_text(&args, 0, "semver_minor")?)
                    .map(|v| SqlValue::Integer(v.minor as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_PATCH => Ok(parse_or_null(&arg_text(&args, 0, "semver_patch")?)
                    .map(|v| SqlValue::Integer(v.patch as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_PRE => Ok(parse_or_null(&arg_text(&args, 0, "semver_pre")?)
                    .map(|v| {
                        if v.pre.is_empty() {
                            SqlValue::Null
                        } else {
                            SqlValue::Text(v.pre.to_string())
                        }
                    })
                    .unwrap_or(SqlValue::Null)),
                FID_BUILD => Ok(parse_or_null(&arg_text(&args, 0, "semver_build")?)
                    .map(|v| {
                        if v.build.is_empty() {
                            SqlValue::Null
                        } else {
                            SqlValue::Text(v.build.to_string())
                        }
                    })
                    .unwrap_or(SqlValue::Null)),
                FID_COMPARE => {
                    let a = arg_text(&args, 0, "semver_compare")?;
                    let b = arg_text(&args, 1, "semver_compare")?;
                    let va = Version::parse(&a).map_err(|e| format!("semver_compare a: {e}"))?;
                    let vb = Version::parse(&b).map_err(|e| format!("semver_compare b: {e}"))?;
                    Ok(SqlValue::Integer(match va.cmp(&vb) {
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
                                SqlValue::Text(a)
                            } else {
                                SqlValue::Text(b)
                            }
                        }
                        (Some(_), None) => SqlValue::Text(a),
                        (None, Some(_)) => SqlValue::Text(b),
                        (None, None) => SqlValue::Null,
                    })
                }
                FID_SATISFIES => {
                    let v = arg_text(&args, 0, "semver_satisfies")?;
                    let req = arg_text(&args, 1, "semver_satisfies")?;
                    let parsed_v = parse_or_null(&v);
                    let parsed_req = VersionReq::from_str(&req).ok();
                    Ok(match (parsed_v, parsed_req) {
                        (Some(vv), Some(rr)) => SqlValue::Integer(rr.matches(&vv) as i64),
                        _ => SqlValue::Null,
                    })
                }
                FID_INCREMENT => {
                    let v = arg_text(&args, 0, "semver_increment")?;
                    let part = arg_text(&args, 1, "semver_increment")?;
                    let mut parsed = match parse_or_null(&v) {
                        Some(p) => p,
                        None => return Ok(SqlValue::Null),
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
                    Ok(SqlValue::Text(parsed.to_string()))
                }
                other => Err(format!("semver: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
