//! Authenticated encryption with associated data (AEAD) for SQL.
//!
//! Wraps the RustCrypto AEAD trait with two algorithms  AES-GCM-256
//! and ChaCha20-Poly1305  exposed as scalar functions. Both take a
//! 32-byte key + 12-byte nonce and return ciphertext in combined
//! form (`ciphertext || tag` where tag is 16 bytes). The associated
//! data argument is optional; passing NULL is equivalent to no AAD.
//!
//! Decrypt returns NULL on any verification failure. This matches
//! the contract callers actually want from SQL  "did this decrypt
//! cleanly?" is best expressed as a nullable value, not a try/catch
//! on a per-row error. The four failure modes (wrong key, tampered
//! ciphertext, wrong nonce, wrong aad) are indistinguishable by
//! design  exposing which one failed would leak side-channel info.
//!
//! Random helpers (`aead_random_key_256`, `aead_random_nonce_96`)
//! pull bytes from `wasi:random/random` via `getrandom`'s `wasi`
//! feature; the WASI reactor adapter routes this to the host. Both
//! are flagged non-deterministic so SQLite won't fold them.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce as AesNonce};
    use chacha20poly1305::{ChaCha20Poly1305, Nonce as ChaNonce};

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

    // ---- Function IDs ----
    const FID_AES_ENCRYPT: u64 = 1;
    const FID_AES_DECRYPT: u64 = 2;
    const FID_CHA_ENCRYPT: u64 = 3;
    const FID_CHA_DECRYPT: u64 = 4;
    const FID_RANDOM_KEY: u64 = 5;
    const FID_RANDOM_NONCE: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

    // ---- Arg helpers ----

    /// Accept BLOB or TEXT (UTF-8 bytes) at position `i`. Crypto
    /// keys / nonces / ciphertexts are most naturally BLOBs but TEXT
    /// passphrases are convenient too. Reject INTEGER / REAL / NULL
    /// (NULL is handled separately for the optional aad slot).
    fn arg_bytes(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            Some(SqlValue::Null) => Err(format!("{fname}: arg {i} is NULL")),
            _ => Err(format!("{fname}: arg {i} must be BLOB or TEXT")),
        }
    }

    /// Optional AAD slot (4th argument). Treat missing or NULL as
    /// no AAD  the cipher is called with an empty `aad` slice,
    /// which matches the standard "no associated data" semantics.
    fn opt_aad(args: &[SqlValue]) -> Vec<u8> {
        match args.get(3) {
            None | Some(SqlValue::Null) => Vec::new(),
            Some(SqlValue::Blob(b)) => b.clone(),
            Some(SqlValue::Text(s)) => s.as_bytes().to_vec(),
            // Defensive: coerce other types to their text form
            // rather than erroring. AAD is opaque bytes either way
            // and rejecting INTEGER here would be surprising.
            Some(SqlValue::Integer(n)) => n.to_string().into_bytes(),
            Some(SqlValue::Real(r)) => r.to_string().into_bytes(),
        }
    }

    fn check_key(key: &[u8], fname: &str) -> Result<[u8; 32], String> {
        key.try_into()
            .map_err(|_| format!("{fname}: key must be 32 bytes, got {}", key.len()))
    }

    fn check_nonce(nonce: &[u8], fname: &str) -> Result<[u8; 12], String> {
        nonce
            .try_into()
            .map_err(|_| format!("{fname}: nonce must be 12 bytes, got {}", nonce.len()))
    }

    // ---- Encrypt / decrypt cores ----

    fn aes_encrypt(key: &[u8; 32], nonce: &[u8; 12], pt: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let cipher = Aes256Gcm::new(key.into());
        cipher
            .encrypt(AesNonce::from_slice(nonce), Payload { msg: pt, aad })
            .map_err(|e| format!("aes_gcm_encrypt: {e}"))
    }

    /// Returns None on any auth/decrypt failure. The exact error is
    /// intentionally collapsed  see module docstring.
    fn aes_decrypt(key: &[u8; 32], nonce: &[u8; 12], ct: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        let cipher = Aes256Gcm::new(key.into());
        cipher
            .decrypt(AesNonce::from_slice(nonce), Payload { msg: ct, aad })
            .ok()
    }

    fn cha_encrypt(key: &[u8; 32], nonce: &[u8; 12], pt: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let cipher = ChaCha20Poly1305::new(key.into());
        cipher
            .encrypt(ChaNonce::from_slice(nonce), Payload { msg: pt, aad })
            .map_err(|e| format!("chacha20_poly1305_encrypt: {e}"))
    }

    fn cha_decrypt(key: &[u8; 32], nonce: &[u8; 12], ct: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(key.into());
        cipher
            .decrypt(ChaNonce::from_slice(nonce), Payload { msg: ct, aad })
            .ok()
    }

    // ---- Random helpers ----

    /// Fill `n` bytes via getrandom. The `wasi` feature routes this
    /// through wasi:random/random in the reactor adapter. The host
    /// is always able to satisfy random reads, so an Err here would
    /// indicate a broken component runtime  surface it as an error.
    fn random_blob(n: usize, fname: &str) -> Result<Vec<u8>, String> {
        let mut out = alloc::vec![0u8; n];
        getrandom::getrandom(&mut out).map_err(|e| format!("{fname}: {e}"))?;
        Ok(out)
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            // Random sources must not be folded across rows.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "aead".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // num_args = -1  variadic; we accept 3 or 4 args
                    // (the optional AAD slot).
                    s(FID_AES_ENCRYPT, "aes_gcm_encrypt", -1, det),
                    s(FID_AES_DECRYPT, "aes_gcm_decrypt", -1, det),
                    s(FID_CHA_ENCRYPT, "chacha20_poly1305_encrypt", -1, det),
                    s(FID_CHA_DECRYPT, "chacha20_poly1305_decrypt", -1, det),
                    s(FID_RANDOM_KEY, "aead_random_key_256", 0, nd),
                    s(FID_RANDOM_NONCE, "aead_random_nonce_96", 0, nd),
                    s(FID_VERSION, "aead_version", 0, det),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_AES_ENCRYPT => {
                    let key = arg_bytes(&args, 0, "aes_gcm_encrypt")?;
                    let pt = arg_bytes(&args, 1, "aes_gcm_encrypt")?;
                    let nonce = arg_bytes(&args, 2, "aes_gcm_encrypt")?;
                    let aad = opt_aad(&args);
                    let k = check_key(&key, "aes_gcm_encrypt")?;
                    let n = check_nonce(&nonce, "aes_gcm_encrypt")?;
                    aes_encrypt(&k, &n, &pt, &aad).map(SqlValue::Blob)
                }
                FID_AES_DECRYPT => {
                    let key = arg_bytes(&args, 0, "aes_gcm_decrypt")?;
                    let ct = arg_bytes(&args, 1, "aes_gcm_decrypt")?;
                    let nonce = arg_bytes(&args, 2, "aes_gcm_decrypt")?;
                    let aad = opt_aad(&args);
                    // Key / nonce length errors collapse to NULL.
                    // Rationale: same SQL contract  "did this
                    // decrypt cleanly?"  is no for any malformed
                    // input. Avoids try/catch on attacker-controlled
                    // row data.
                    let k = match check_key(&key, "aes_gcm_decrypt") {
                        Ok(k) => k,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    let n = match check_nonce(&nonce, "aes_gcm_decrypt") {
                        Ok(n) => n,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    Ok(match aes_decrypt(&k, &n, &ct, &aad) {
                        Some(pt) => SqlValue::Blob(pt),
                        None => SqlValue::Null,
                    })
                }
                FID_CHA_ENCRYPT => {
                    let key = arg_bytes(&args, 0, "chacha20_poly1305_encrypt")?;
                    let pt = arg_bytes(&args, 1, "chacha20_poly1305_encrypt")?;
                    let nonce = arg_bytes(&args, 2, "chacha20_poly1305_encrypt")?;
                    let aad = opt_aad(&args);
                    let k = check_key(&key, "chacha20_poly1305_encrypt")?;
                    let n = check_nonce(&nonce, "chacha20_poly1305_encrypt")?;
                    cha_encrypt(&k, &n, &pt, &aad).map(SqlValue::Blob)
                }
                FID_CHA_DECRYPT => {
                    let key = arg_bytes(&args, 0, "chacha20_poly1305_decrypt")?;
                    let ct = arg_bytes(&args, 1, "chacha20_poly1305_decrypt")?;
                    let nonce = arg_bytes(&args, 2, "chacha20_poly1305_decrypt")?;
                    let aad = opt_aad(&args);
                    let k = match check_key(&key, "chacha20_poly1305_decrypt") {
                        Ok(k) => k,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    let n = match check_nonce(&nonce, "chacha20_poly1305_decrypt") {
                        Ok(n) => n,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    Ok(match cha_decrypt(&k, &n, &ct, &aad) {
                        Some(pt) => SqlValue::Blob(pt),
                        None => SqlValue::Null,
                    })
                }
                FID_RANDOM_KEY => random_blob(32, "aead_random_key_256").map(SqlValue::Blob),
                FID_RANDOM_NONCE => random_blob(12, "aead_random_nonce_96").map(SqlValue::Blob),
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("aead: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
