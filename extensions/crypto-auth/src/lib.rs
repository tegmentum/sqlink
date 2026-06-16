//! Authentication primitives: JWT, TOTP, Argon2, bcrypt.

extern crate alloc;

use alloc::string::{String, ToString};

// ───────────── JWT ─────────────

pub fn jwt_verify(token: &str, secret: &str) -> bool {
    use jsonwebtoken::{decode, DecodingKey, Validation};
    let mut v = Validation::new(jsonwebtoken::Algorithm::HS256);
    // Don't reject on missing claims  the function is just
    // "signature OK and not expired" by default.
    v.required_spec_claims.clear();
    let key = DecodingKey::from_secret(secret.as_bytes());
    decode::<serde_json::Value>(token, &key, &v).is_ok()
}

pub fn jwt_decode_header(token: &str) -> Result<String, String> {
    let header = jsonwebtoken::decode_header(token)
        .map_err(|e| alloc::format!("jwt_decode_header: {e}"))?;
    let j = serde_json::to_string(&header)
        .map_err(|e| alloc::format!("jwt_decode_header: serialize: {e}"))?;
    Ok(j)
}

pub fn jwt_decode_payload(token: &str) -> Result<String, String> {
    // The payload is the 2nd base64url segment. We don't
    // validate the signature here  callers use jwt_verify for
    // that. Pull the segment, base64url-decode, parse as JSON.
    let parts: alloc::vec::Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return Err("jwt_decode_payload: malformed token".into());
    }
    let raw = base64_url_decode(parts[1])?;
    let s = core::str::from_utf8(&raw)
        .map_err(|e| alloc::format!("jwt_decode_payload: UTF-8: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(s)
        .map_err(|e| alloc::format!("jwt_decode_payload: JSON: {e}"))?;
    Ok(v.to_string())
}

fn base64_url_decode(s: &str) -> Result<alloc::vec::Vec<u8>, String> {
    // Pad to a multiple of 4.
    let pad_len = (4 - s.len() % 4) % 4;
    let mut buf = String::with_capacity(s.len() + pad_len);
    buf.push_str(s);
    for _ in 0..pad_len {
        buf.push('=');
    }
    // Map URL-safe chars to standard.
    let s = buf.replace('-', "+").replace('_', "/");
    use base64_engine::Engine as _;
    base64_engine::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .map_err(|e| alloc::format!("base64 decode: {e}"))
}

// jsonwebtoken brings in a base64 crate transitively. Shadow
// the path so the no-std path here doesn't conflict.
use base64 as base64_engine;

// ───────────── TOTP ─────────────

pub fn totp(secret_b32: &str, time_unix: u64, period: u64, digits: u32) -> Result<String, String> {
    use hmac::Mac;
    let secret = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, secret_b32)
        .ok_or_else(|| "totp: secret not base32".to_string())?;
    let counter = time_unix / period;
    let mut mac = <hmac::Hmac<sha1::Sha1>>::new_from_slice(&secret)
        .map_err(|e| alloc::format!("totp: hmac key: {e}"))?;
    mac.update(&counter.to_be_bytes());
    let bytes = mac.finalize().into_bytes();
    let offset = (bytes[19] & 0x0f) as usize;
    let bin_code = ((bytes[offset] as u32 & 0x7f) << 24)
        | ((bytes[offset + 1] as u32) << 16)
        | ((bytes[offset + 2] as u32) << 8)
        | (bytes[offset + 3] as u32);
    let modulo = 10u32.pow(digits);
    let code = bin_code % modulo;
    Ok(alloc::format!("{:0width$}", code, width = digits as usize))
}

pub fn totp_verify(
    secret_b32: &str,
    code: &str,
    time_unix: u64,
    window: i32,
    period: u64,
    digits: u32,
) -> Result<bool, String> {
    for step in -window..=window {
        let t = if step < 0 {
            time_unix.saturating_sub((-step as u64) * period)
        } else {
            time_unix.saturating_add(step as u64 * period)
        };
        let c = totp(secret_b32, t, period, digits)?;
        if c == code {
            return Ok(true);
        }
    }
    Ok(false)
}

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

// ───────────── bcrypt ─────────────

pub fn bcrypt_hash(password: &str, cost: u32) -> Result<String, String> {
    bcrypt::hash(password, cost).map_err(|e| alloc::format!("bcrypt_hash: {e}"))
}

pub fn bcrypt_verify(hash: &str, password: &str) -> bool {
    bcrypt::verify(password, hash).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tests rely on platform-native rng for argon2; native
    // builds have it; wasm has wasi-snapshot-preview1::random_get
    // via argon2's getrandom usage.

    #[test]
    fn totp_known_vector_30sec_period_8digits() {
        // RFC 6238 test vectors for SHA-1, with secret = ASCII
        // "12345678901234567890" base32-encoded.
        let secret_b32 = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        // Test vector at T=59 yields '94287082'.
        let code = totp(secret_b32, 59, 30, 8).unwrap();
        assert_eq!(code, "94287082");
        // T=1111111109 yields '07081804'.
        let code = totp(secret_b32, 1_111_111_109, 30, 8).unwrap();
        assert_eq!(code, "07081804");
    }

    #[test]
    fn totp_verify_window_matches_recent() {
        let secret = "JBSWY3DPEHPK3PXP";
        let now = 100_000u64;
        let code = totp(secret, now, 30, 6).unwrap();
        // Same code passes at exact time.
        assert!(totp_verify(secret, &code, now, 0, 30, 6).unwrap());
        // Generated 1 step ago, accepted with window=1.
        let ago = code.clone();
        let _ = ago;
        let later = totp(secret, now + 30, 30, 6).unwrap();
        assert!(totp_verify(secret, &code, now + 30, 1, 30, 6).unwrap());
        assert!(totp_verify(secret, &later, now, 1, 30, 6).unwrap());
    }

    #[test]
    fn argon2_round_trip() {
        let h = argon2_hash("hunter2").unwrap();
        assert!(argon2_verify(&h, "hunter2"));
        assert!(!argon2_verify(&h, "wrong"));
    }

    #[test]
    fn bcrypt_round_trip() {
        let h = bcrypt_hash("hunter2", 4).unwrap(); // low cost  fast test
        assert!(bcrypt_verify(&h, "hunter2"));
        assert!(!bcrypt_verify(&h, "wrong"));
    }

    #[test]
    fn jwt_verify_round_trip() {
        use jsonwebtoken::{encode, EncodingKey, Header};
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Claims {
            sub: String,
            exp: u64,
        }
        let claims = Claims {
            sub: "alice".into(),
            // Far-future expiry so the test stays valid.
            exp: 99_999_999_999,
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"hunter2"),
        )
        .unwrap();
        assert!(jwt_verify(&token, "hunter2"));
        assert!(!jwt_verify(&token, "wrong"));
    }

    #[test]
    fn jwt_decode_payload_works() {
        use jsonwebtoken::{encode, EncodingKey, Header};
        let claims = serde_json::json!({"sub": "alice", "iat": 1234});
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"k"),
        )
        .unwrap();
        let payload = jwt_decode_payload(&token).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["sub"], "alice");
        assert_eq!(parsed["iat"], 1234);
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

    const FID_JWT_VERIFY: u64 = 1;
    const FID_JWT_HEADER: u64 = 2;
    const FID_JWT_PAYLOAD: u64 = 3;
    const FID_TOTP_2: u64 = 4;
    const FID_TOTP_4: u64 = 5;
    const FID_TOTP_VERIFY_4: u64 = 6;
    const FID_TOTP_VERIFY_6: u64 = 7;
    const FID_ARGON2_HASH: u64 = 8;
    const FID_ARGON2_VERIFY: u64 = 9;
    const FID_BCRYPT_HASH_1: u64 = 10;
    const FID_BCRYPT_HASH_2: u64 = 11;
    const FID_BCRYPT_VERIFY: u64 = 12;
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
                    s(FID_JWT_VERIFY, "jwt_verify", 2, det),
                    s(FID_JWT_HEADER, "jwt_decode_header", 1, det),
                    s(FID_JWT_PAYLOAD, "jwt_decode_payload", 1, det),
                    // totp(secret, time) defaults to (30, 6).
                    s(FID_TOTP_2, "totp", 2, det),
                    // totp(secret, time, period, digits)  full form.
                    s(FID_TOTP_4, "totp", 4, det),
                    // totp_verify(secret, code, time, window)
                    s(FID_TOTP_VERIFY_4, "totp_verify", 4, det),
                    // totp_verify(secret, code, time, window, period, digits)
                    s(FID_TOTP_VERIFY_6, "totp_verify", 6, det),
                    // argon2_hash/verify use random salts, so the
                    // hash side is non-deterministic.
                    s(FID_ARGON2_HASH, "argon2_hash", 1, nd),
                    s(FID_ARGON2_VERIFY, "argon2_verify", 2, det),
                    s(FID_BCRYPT_HASH_1, "bcrypt_hash", 1, nd),
                    s(FID_BCRYPT_HASH_2, "bcrypt_hash", 2, nd),
                    s(FID_BCRYPT_VERIFY, "bcrypt_verify", 2, det),
                    s(FID_VERSION, "crypto_auth_version", 0, nd),
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
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_JWT_VERIFY => {
                    let token = arg_text(&args, 0, "jwt_verify")?;
                    let secret = arg_text(&args, 1, "jwt_verify")?;
                    Ok(SqlValue::Integer(super::jwt_verify(&token, &secret) as i64))
                }
                FID_JWT_HEADER => {
                    let t = arg_text(&args, 0, "jwt_decode_header")?;
                    super::jwt_decode_header(&t).map(SqlValue::Text)
                }
                FID_JWT_PAYLOAD => {
                    let t = arg_text(&args, 0, "jwt_decode_payload")?;
                    super::jwt_decode_payload(&t).map(SqlValue::Text)
                }
                FID_TOTP_2 | FID_TOTP_4 => {
                    let secret = arg_text(&args, 0, "totp")?;
                    let time = arg_int(&args, 1, "totp")? as u64;
                    let (period, digits) = if func_id == FID_TOTP_4 {
                        (arg_int(&args, 2, "totp")? as u64, arg_int(&args, 3, "totp")? as u32)
                    } else {
                        (30, 6)
                    };
                    super::totp(&secret, time, period, digits).map(SqlValue::Text)
                }
                FID_TOTP_VERIFY_4 | FID_TOTP_VERIFY_6 => {
                    let secret = arg_text(&args, 0, "totp_verify")?;
                    let code = arg_text(&args, 1, "totp_verify")?;
                    let time = arg_int(&args, 2, "totp_verify")? as u64;
                    let window = arg_int(&args, 3, "totp_verify")? as i32;
                    let (period, digits) = if func_id == FID_TOTP_VERIFY_6 {
                        (
                            arg_int(&args, 4, "totp_verify")? as u64,
                            arg_int(&args, 5, "totp_verify")? as u32,
                        )
                    } else {
                        (30, 6)
                    };
                    super::totp_verify(&secret, &code, time, window, period, digits)
                        .map(|b| SqlValue::Integer(b as i64))
                }
                FID_ARGON2_HASH => {
                    let p = arg_text(&args, 0, "argon2_hash")?;
                    super::argon2_hash(&p).map(SqlValue::Text)
                }
                FID_ARGON2_VERIFY => {
                    let h = arg_text(&args, 0, "argon2_verify")?;
                    let p = arg_text(&args, 1, "argon2_verify")?;
                    Ok(SqlValue::Integer(super::argon2_verify(&h, &p) as i64))
                }
                FID_BCRYPT_HASH_1 | FID_BCRYPT_HASH_2 => {
                    let p = arg_text(&args, 0, "bcrypt_hash")?;
                    let cost = if func_id == FID_BCRYPT_HASH_2 {
                        arg_int(&args, 1, "bcrypt_hash")? as u32
                    } else {
                        12
                    };
                    super::bcrypt_hash(&p, cost).map(SqlValue::Text)
                }
                FID_BCRYPT_VERIFY => {
                    let h = arg_text(&args, 0, "bcrypt_verify")?;
                    let p = arg_text(&args, 1, "bcrypt_verify")?;
                    Ok(SqlValue::Integer(super::bcrypt_verify(&h, &p) as i64))
                }
                other => Err(format!("crypto-auth: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
