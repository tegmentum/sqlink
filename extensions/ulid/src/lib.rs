//! ULID (Universally Unique Lexicographically Sortable Identifier)
//! scalar functions for SQLite.
//!
//! ULID layout (128 bits):
//!   bits 127..80 — 48-bit ms epoch timestamp (big-endian)
//!   bits  79..0  — 80-bit randomness
//!
//! Encoded as 26 chars of Crockford base32 (text) or 16 raw big-endian
//! bytes (blob). Different operating point from uuid v4: lexicographic
//! sort = time sort, so ULID is a better primary key when you want
//! recent-rows queries cheap.
//!
//! Exposes:
//!   ulid()                   -> text  (26 chars, Crockford base32)
//!   ulid_blob()              -> blob  (16 bytes, big-endian)
//!   ulid_from(epoch_ms)      -> text  (encode a specific timestamp)
//!   ulid_timestamp(ulid)     -> int   (extract ms epoch, NULL on parse fail)
//!   ulid_random_part(ulid)   -> blob  (10 bytes, NULL on parse fail)
//!
//! Generators are flagged non-deterministic so the planner can't
//! hoist them. `ulid_timestamp` and `ulid_random_part` are pure
//! functions of their input  marked DETERMINISTIC. `ulid_from` is
//! also deterministic-ish (same ts in  same time portion, but the
//! 80 random bits differ each call), so it's flagged non-det.

extern crate alloc;

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

    use ulid::Ulid;

    const FID_ULID:        u64 = 1;
    const FID_ULID_BLOB:   u64 = 2;
    const FID_ULID_FROM:   u64 = 3;
    const FID_ULID_TS:     u64 = 4;
    const FID_ULID_RANDOM: u64 = 5;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    /// Build a Ulid whose timestamp portion is `ms` and whose random
    /// portion is freshly drawn. Implementation trick: ask the ulid
    /// crate for a fresh Ulid (which internally pulls from the OS
    /// rng via getrandom  on wasi-p2 that's wasi-random), then
    /// substitute our target timestamp via `from_parts`. Keeps us
    /// from pulling `rand` in directly as a dep.
    fn ulid_from_ms(ms: u64) -> Ulid {
        let seed = Ulid::new();
        Ulid::from_parts(ms, seed.random())
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let nd = FunctionFlags::empty();
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "ulid".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // Generators  non-deterministic so the planner
                    // can't hoist (each row needs its own id).
                    s(FID_ULID,        "ulid",            0, nd),
                    s(FID_ULID_BLOB,   "ulid_blob",       0, nd),
                    // ulid_from(ms) generates a fresh random portion
                    // each call, so still non-deterministic in output
                    // even though the timestamp piece is pinned.
                    s(FID_ULID_FROM,   "ulid_from",       1, nd),
                    // Parsers / extractors  pure functions of input.
                    s(FID_ULID_TS,     "ulid_timestamp",  1, det),
                    s(FID_ULID_RANDOM, "ulid_random_part", 1, det),
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
                preferred_prefix: Some("ulid".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.ulid".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_ULID => Ok(SqlValue::Text(Ulid::new().to_string())),
                FID_ULID_BLOB => Ok(SqlValue::Blob(Ulid::new().to_bytes().to_vec())),
                FID_ULID_FROM => {
                    let ms = arg_int(&args, 0, "ulid_from")?;
                    if ms < 0 {
                        return Err("ulid_from: epoch_ms must be non-negative".into());
                    }
                    Ok(SqlValue::Text(ulid_from_ms(ms as u64).to_string()))
                }
                FID_ULID_TS => {
                    let t = arg_text(&args, 0, "ulid_timestamp")?;
                    Ok(Ulid::from_string(&t)
                        .ok()
                        .map(|u| SqlValue::Integer(u.timestamp_ms() as i64))
                        .unwrap_or(SqlValue::Null))
                }
                FID_ULID_RANDOM => {
                    let t = arg_text(&args, 0, "ulid_random_part")?;
                    Ok(Ulid::from_string(&t)
                        .ok()
                        .map(|u| {
                            // Lower 80 bits of the 128-bit value.
                            // Encode as 10 BE bytes  matches the
                            // ULID spec's "random" half exactly.
                            let raw: u128 = u.random();
                            let mut out = [0u8; 10];
                            for i in 0..10 {
                                out[9 - i] = ((raw >> (i * 8)) & 0xff) as u8;
                            }
                            SqlValue::Blob(out.to_vec())
                        })
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("ulid: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
