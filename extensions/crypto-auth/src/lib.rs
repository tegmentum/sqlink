//! Authentication primitives: Argon2.
//!
//! Historically this umbrella also exposed JWT, TOTP, and bcrypt
//! functions, but those have been split out into focused
//! extensions (jwt, totp, pwhash). This crate now retains only
//! Argon2 password hashing.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::{String, ToString};

// ───────────── Argon2 ─────────────

pub fn argon2_hash(password: &str) -> Result<String, String> {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    use argon2::Argon2;
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| alloc::format!("argon2_hash: {e}"))
}

pub fn argon2_verify(hash: &str, password: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    use argon2::Argon2;
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_round_trip() {
        let h = argon2_hash("hunter2").unwrap();
        assert!(argon2_verify(&h, "hunter2"));
        assert!(!argon2_verify(&h, "wrong"));
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

    const FID_ARGON2_HASH: u64 = 8;
    const FID_ARGON2_VERIFY: u64 = 9;
    const FID_VERSION: u64 = 13;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: f,
            };
            Manifest {
                name: "crypto-auth".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // argon2_hash uses random salts, so the
                    // hash side is non-deterministic.
                    s(FID_ARGON2_HASH, "argon2_hash", 1, nd),
                    s(FID_ARGON2_VERIFY, "argon2_verify", 2, det),
                    s(FID_VERSION, "crypto_auth_version", 0, nd),
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
            }
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_ARGON2_HASH => {
                    let p = arg_text(&args, 0, "argon2_hash")?;
                    super::argon2_hash(&p).map(SqlValue::Text)
                }
                FID_ARGON2_VERIFY => {
                    let h = arg_text(&args, 0, "argon2_verify")?;
                    let p = arg_text(&args, 1, "argon2_verify")?;
                    Ok(SqlValue::Integer(super::argon2_verify(&h, &p) as i64))
                }
                other => Err(format!("crypto-auth: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
