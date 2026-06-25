//! HKDF (HMAC-based Extract-and-Expand Key Derivation Function)
//! for SQLite. RFC 5869.
//!
//! The right primitive for "I have N bytes of shared secret; give
//! me a key for AES-GCM, a key for ChaCha, and an HMAC key derived
//! from it" -- exactly what the `aead` extension wants upstream.
//!
//! Function surface (PLAN-more-extensions-5.md #2):
//!
//!   hkdf_sha256(ikm, salt, info, length)   -> blob (length bytes)
//!   hkdf_sha512(ikm, salt, info, length)   -> blob
//!   hkdf_sha256_extract(ikm, salt)         -> blob (32-byte PRK)
//!   hkdf_sha512_extract(ikm, salt)         -> blob (64-byte PRK)
//!   hkdf_sha256_expand(prk, info, length)  -> blob
//!   hkdf_sha512_expand(prk, info, length)  -> blob
//!   hkdf_version()                         -> text
//!
//! Argument coercion:
//!   * `ikm`, `salt`, `info`, `prk`: TEXT -> utf-8 bytes, BLOB as-is,
//!     NULL -> empty (RFC 5869 explicitly allows zero-length salt
//!     and info; an absent salt is treated as HashLen zero bytes by
//!     the hkdf crate).
//!   * `length`: must be a positive INTEGER in 1..=255*HashLen
//!     (255*32 = 8160 for SHA-256, 255*64 = 16320 for SHA-512).
//!     Out-of-range or non-INTEGER -> NULL (degenerate request, not
//!     an error -- caller can SELECT and check IS NULL).
//!   * `prk` for `*_expand`: TEXT or BLOB. Wrong length surfaces as
//!     NULL (the hkdf crate's `from_prk` rejects PRKs shorter than
//!     HashLen; we let that propagate).
//!
//! NULL on ikm/salt/info is explicitly NOT NULL on output -- HKDF
//! is well-defined with empty inputs, and SQL callers that want
//! "skip if no ikm" can wrap in `CASE WHEN ikm IS NOT NULL`.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
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

    use hkdf::Hkdf;
    use sha2::{Sha256, Sha512};

    const FID_SHA256: u64 = 1;
    const FID_SHA512: u64 = 2;
    const FID_SHA256_EXTRACT: u64 = 3;
    const FID_SHA512_EXTRACT: u64 = 4;
    const FID_SHA256_EXPAND: u64 = 5;
    const FID_SHA512_EXPAND: u64 = 6;
    const FID_VERSION: u64 = 7;

    /// SHA-256 output size (bytes). RFC 5869 caps OKM at 255 *
    /// HashLen, so this also bounds the `length` argument.
    const SHA256_LEN: usize = 32;
    const SHA512_LEN: usize = 64;
    const SHA256_MAX_OKM: usize = 255 * SHA256_LEN; // 8160
    const SHA512_MAX_OKM: usize = 255 * SHA512_LEN; // 16320

    struct Ext;

    /// Coerce SqlValue -> bytes. Matches the rest of the crypto
    /// catalog (jwt, blake3, aead): TEXT -> utf-8, BLOB as-is,
    /// NULL -> empty. INTEGER/REAL are rejected -- a typo'd
    /// numeric `ikm` is almost certainly a bug.
    fn bytes_of(v: &SqlValue, name: &str, role: &str) -> Result<Vec<u8>, String> {
        match v {
            SqlValue::Text(s) => Ok(s.as_bytes().to_vec()),
            SqlValue::Blob(b) => Ok(b.clone()),
            SqlValue::Null => Ok(Vec::new()),
            _ => Err(format!("{name}: {role} must be TEXT, BLOB, or NULL")),
        }
    }

    /// Validate the length arg. Returns None (NULL output) on
    /// out-of-range or non-INTEGER input -- the RFC defines a hard
    /// upper bound and asking for zero bytes is degenerate.
    fn parse_length(v: Option<&SqlValue>, max: usize) -> Option<usize> {
        match v? {
            SqlValue::Integer(n) => {
                if *n < 1 {
                    return None;
                }
                let n = *n as usize;
                if n > max {
                    return None;
                }
                Some(n)
            }
            _ => None,
        }
    }

    /// One-shot HKDF: `hkdf_sha256(ikm, salt, info, length)`.
    /// Returns NULL when `length` is invalid (the only NULL path --
    /// HKDF itself succeeds for any byte combination of ikm/salt/info).
    fn hkdf_sha256_full(
        ikm: &[u8],
        salt: &[u8],
        info: &[u8],
        len: usize,
    ) -> Result<Vec<u8>, String> {
        // Hkdf::new treats an empty salt as the all-zeros HashLen
        // salt per RFC 5869 § 2.2; pass None when explicitly empty
        // to get that behavior.
        let salt_opt = if salt.is_empty() { None } else { Some(salt) };
        let hk = Hkdf::<Sha256>::new(salt_opt, ikm);
        let mut okm = vec![0u8; len];
        hk.expand(info, &mut okm)
            .map_err(|e| format!("hkdf_sha256: expand: {e}"))?;
        Ok(okm)
    }

    fn hkdf_sha512_full(
        ikm: &[u8],
        salt: &[u8],
        info: &[u8],
        len: usize,
    ) -> Result<Vec<u8>, String> {
        let salt_opt = if salt.is_empty() { None } else { Some(salt) };
        let hk = Hkdf::<Sha512>::new(salt_opt, ikm);
        let mut okm = vec![0u8; len];
        hk.expand(info, &mut okm)
            .map_err(|e| format!("hkdf_sha512: expand: {e}"))?;
        Ok(okm)
    }

    /// Explicit extract step: returns the PRK only (HashLen bytes).
    /// Useful when the same PRK is reused across many expands.
    fn hkdf_sha256_extract(ikm: &[u8], salt: &[u8]) -> Vec<u8> {
        let salt_opt = if salt.is_empty() { None } else { Some(salt) };
        let (prk, _hk) = Hkdf::<Sha256>::extract(salt_opt, ikm);
        prk.to_vec()
    }

    fn hkdf_sha512_extract(ikm: &[u8], salt: &[u8]) -> Vec<u8> {
        let salt_opt = if salt.is_empty() { None } else { Some(salt) };
        let (prk, _hk) = Hkdf::<Sha512>::extract(salt_opt, ikm);
        prk.to_vec()
    }

    /// Explicit expand step: PRK in, OKM out. PRK must be at least
    /// HashLen bytes; shorter -> NULL.
    fn hkdf_sha256_expand(prk: &[u8], info: &[u8], len: usize) -> Option<Vec<u8>> {
        let hk = Hkdf::<Sha256>::from_prk(prk).ok()?;
        let mut okm = vec![0u8; len];
        hk.expand(info, &mut okm).ok()?;
        Some(okm)
    }

    fn hkdf_sha512_expand(prk: &[u8], info: &[u8], len: usize) -> Option<Vec<u8>> {
        let hk = Hkdf::<Sha512>::from_prk(prk).ok()?;
        let mut okm = vec![0u8; len];
        hk.expand(info, &mut okm).ok()?;
        Some(okm)
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "hkdf".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_SHA256, "hkdf_sha256", 4, det),
                    s(FID_SHA512, "hkdf_sha512", 4, det),
                    s(FID_SHA256_EXTRACT, "hkdf_sha256_extract", 2, det),
                    s(FID_SHA512_EXTRACT, "hkdf_sha512_extract", 2, det),
                    s(FID_SHA256_EXPAND, "hkdf_sha256_expand", 3, det),
                    s(FID_SHA512_EXPAND, "hkdf_sha512_expand", 3, det),
                    s(FID_VERSION, "hkdf_version", 0, det),
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
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_SHA256 => {
                    let ikm = bytes_of(
                        args.first().ok_or("hkdf_sha256: missing ikm")?,
                        "hkdf_sha256",
                        "ikm",
                    )?;
                    let salt = bytes_of(
                        args.get(1).ok_or("hkdf_sha256: missing salt")?,
                        "hkdf_sha256",
                        "salt",
                    )?;
                    let info = bytes_of(
                        args.get(2).ok_or("hkdf_sha256: missing info")?,
                        "hkdf_sha256",
                        "info",
                    )?;
                    let len = match parse_length(args.get(3), SHA256_MAX_OKM) {
                        Some(n) => n,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Blob(hkdf_sha256_full(&ikm, &salt, &info, len)?))
                }
                FID_SHA512 => {
                    let ikm = bytes_of(
                        args.first().ok_or("hkdf_sha512: missing ikm")?,
                        "hkdf_sha512",
                        "ikm",
                    )?;
                    let salt = bytes_of(
                        args.get(1).ok_or("hkdf_sha512: missing salt")?,
                        "hkdf_sha512",
                        "salt",
                    )?;
                    let info = bytes_of(
                        args.get(2).ok_or("hkdf_sha512: missing info")?,
                        "hkdf_sha512",
                        "info",
                    )?;
                    let len = match parse_length(args.get(3), SHA512_MAX_OKM) {
                        Some(n) => n,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Blob(hkdf_sha512_full(&ikm, &salt, &info, len)?))
                }
                FID_SHA256_EXTRACT => {
                    let ikm = bytes_of(
                        args.first().ok_or("hkdf_sha256_extract: missing ikm")?,
                        "hkdf_sha256_extract",
                        "ikm",
                    )?;
                    let salt = bytes_of(
                        args.get(1).ok_or("hkdf_sha256_extract: missing salt")?,
                        "hkdf_sha256_extract",
                        "salt",
                    )?;
                    Ok(SqlValue::Blob(hkdf_sha256_extract(&ikm, &salt)))
                }
                FID_SHA512_EXTRACT => {
                    let ikm = bytes_of(
                        args.first().ok_or("hkdf_sha512_extract: missing ikm")?,
                        "hkdf_sha512_extract",
                        "ikm",
                    )?;
                    let salt = bytes_of(
                        args.get(1).ok_or("hkdf_sha512_extract: missing salt")?,
                        "hkdf_sha512_extract",
                        "salt",
                    )?;
                    Ok(SqlValue::Blob(hkdf_sha512_extract(&ikm, &salt)))
                }
                FID_SHA256_EXPAND => {
                    let prk = bytes_of(
                        args.first().ok_or("hkdf_sha256_expand: missing prk")?,
                        "hkdf_sha256_expand",
                        "prk",
                    )?;
                    let info = bytes_of(
                        args.get(1).ok_or("hkdf_sha256_expand: missing info")?,
                        "hkdf_sha256_expand",
                        "info",
                    )?;
                    let len = match parse_length(args.get(2), SHA256_MAX_OKM) {
                        Some(n) => n,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match hkdf_sha256_expand(&prk, &info, len) {
                        Some(okm) => SqlValue::Blob(okm),
                        None => SqlValue::Null,
                    })
                }
                FID_SHA512_EXPAND => {
                    let prk = bytes_of(
                        args.first().ok_or("hkdf_sha512_expand: missing prk")?,
                        "hkdf_sha512_expand",
                        "prk",
                    )?;
                    let info = bytes_of(
                        args.get(1).ok_or("hkdf_sha512_expand: missing info")?,
                        "hkdf_sha512_expand",
                        "info",
                    )?;
                    let len = match parse_length(args.get(2), SHA512_MAX_OKM) {
                        Some(n) => n,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match hkdf_sha512_expand(&prk, &info, len) {
                        Some(okm) => SqlValue::Blob(okm),
                        None => SqlValue::Null,
                    })
                }
                FID_VERSION => {
                    // Surface the extension's own version. The hkdf
                    // crate's version is pinned in Cargo.toml; callers
                    // who need to assert on it can read the manifest.
                    let v = format!(
                        "hkdf crate 0.12 (RFC 5869); extension {}",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }
                other => Err(format!("hkdf: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
