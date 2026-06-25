//! Standalone Ed25519 sign / verify for SQL.
//!
//! Pairs with the `jwt` extension (which has Ed25519 internally for
//! the EdDSA JWS alg) and `secp256k1` (the other curve-25519-adjacent
//! signing primitive in the catalog). The differentiator vs `jwt` is
//! that this surface gives you raw Ed25519 outside the JOSE envelope
//! -- the form SSH, Tor, Signal, Solana, and most modern PKI
//! protocols actually use on the wire.
//!
//! Function surface:
//!
//!   ed25519_keypair()                        -> blob   (64 bytes: 32 priv seed || 32 pub)
//!   ed25519_pub_from_priv(priv_blob)         -> blob   (32-byte pub)
//!   ed25519_sign(priv_blob, message)         -> blob   (64-byte sig)
//!   ed25519_verify(pub_blob, msg, sig_blob)  -> integer 0/1
//!   ed25519_version()                        -> text
//!
//! `priv_blob` is a 32-byte seed (RFC 8032 § 5.1.5 form). For
//! compatibility with libraries that hand out a 64-byte "expanded"
//! secret (seed || pubkey) we also accept 64-byte blobs and use the
//! first 32 bytes as the seed -- matching the `jwt` extension's
//! Ed25519 surface.
//!
//! `message` is the raw bytes to sign. Ed25519 hashes internally
//! (SHA-512 over R || A || M per RFC 8032 § 5.1.6); callers do NOT
//! pre-hash. This is the opposite of secp256k1 in this catalog,
//! where the caller hashes first.
//!
//! NULL passes through as NULL on every fn. Wrong-length / malformed
//! keys + signatures -> NULL (verify -> 0) so the functions compose
//! inside CASE / WHERE without try/catch.
//!
//! RFC 8032 Section 7 vectors are the acceptance check (smoke.sql).

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

    use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey, Signature};

    const FID_KEYPAIR: u64 = 1;
    const FID_PUB_FROM_PRIV: u64 = 2;
    const FID_SIGN: u64 = 3;
    const FID_VERIFY: u64 = 4;
    const FID_VERSION: u64 = 5;

    struct Ext;

    /// Optional-blob coercion: NULL -> None; BLOB / TEXT -> bytes;
    /// INTEGER / REAL rejected (no sensible coercion for binary keys
    /// or signatures). Matches the secp256k1 extension's convention.
    fn opt_blob(args: &[SqlValue], i: usize) -> Result<Option<Vec<u8>>, String> {
        match args.get(i) {
            None => Err(format!("missing arg at {i}")),
            Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Blob(b)) => Ok(Some(b.clone())),
            Some(SqlValue::Text(s)) => Ok(Some(s.as_bytes().to_vec())),
            _ => Err(format!("arg at {i} must be BLOB / TEXT / NULL")),
        }
    }

    /// Parse a private-key blob into a SigningKey.
    ///
    /// Accepts:
    ///   * 32 bytes: raw RFC 8032 seed.
    ///   * 64 bytes: "expanded" secret (seed || pubkey) -- some
    ///     libraries (libsodium's crypto_sign_keypair, Solana,
    ///     Tendermint) emit this form; we use the first 32 bytes.
    ///     Matches the `jwt` extension's Ed25519 surface.
    ///
    /// Any other length -> None (NULL output).
    fn parse_priv(bytes: &[u8]) -> Option<SigningKey> {
        let seed: [u8; 32] = match bytes.len() {
            32 => bytes.try_into().ok()?,
            64 => bytes[..32].try_into().ok()?,
            _ => return None,
        };
        Some(SigningKey::from_bytes(&seed))
    }

    /// Parse a public-key blob into a VerifyingKey. Must be 32 bytes
    /// and represent a valid Edwards-curve point (RFC 8032 § 5.1.3).
    /// Off-curve / non-canonical encodings -> None.
    fn parse_pub(bytes: &[u8]) -> Option<VerifyingKey> {
        let arr: [u8; 32] = bytes.try_into().ok()?;
        VerifyingKey::from_bytes(&arr).ok()
    }

    /// Generate a fresh keypair: 32-byte seed || 32-byte pubkey =
    /// 64 bytes. Randomness routes through getrandom ->
    /// wasi:random/random.
    fn gen_keypair() -> Result<Vec<u8>, String> {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed)
            .map_err(|e| format!("ed25519_keypair: rng: {e}"))?;
        let sk = SigningKey::from_bytes(&seed);
        let pk = sk.verifying_key();
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(&seed);
        out.extend_from_slice(pk.as_bytes());
        Ok(out)
    }

    fn sign(sk: &SigningKey, msg: &[u8]) -> Vec<u8> {
        let sig: Signature = sk.sign(msg);
        sig.to_bytes().to_vec()
    }

    fn verify(pk: &VerifyingKey, msg: &[u8], sig_bytes: &[u8]) -> bool {
        let arr: [u8; 64] = match sig_bytes.try_into() {
            Ok(a) => a,
            Err(_) => return false,
        };
        let sig = Signature::from_bytes(&arr);
        pk.verify(msg, &sig).is_ok()
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
                name: "ed25519".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_KEYPAIR, "ed25519_keypair", 0, nd),
                    s(FID_PUB_FROM_PRIV, "ed25519_pub_from_priv", 1, det),
                    s(FID_SIGN, "ed25519_sign", 2, det),
                    s(FID_VERIFY, "ed25519_verify", 3, det),
                    s(FID_VERSION, "ed25519_version", 0, det),
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
                FID_KEYPAIR => Ok(SqlValue::Blob(gen_keypair()?)),

                FID_PUB_FROM_PRIV => {
                    let Some(b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match parse_priv(&b) {
                        Some(sk) => SqlValue::Blob(sk.verifying_key().as_bytes().to_vec()),
                        None => SqlValue::Null,
                    })
                }

                FID_SIGN => {
                    let Some(priv_b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(msg_b) = opt_blob(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(sk) = parse_priv(&priv_b) else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Blob(sign(&sk, &msg_b)))
                }

                FID_VERIFY => {
                    let Some(pub_b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(msg_b) = opt_blob(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(sig_b) = opt_blob(&args, 2)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(pk) = parse_pub(&pub_b) else {
                        return Ok(SqlValue::Integer(0));
                    };
                    Ok(SqlValue::Integer(verify(&pk, &msg_b, &sig_b) as i64))
                }

                FID_VERSION => {
                    let v = format!(
                        "ed25519-dalek 2 (RFC 8032); extension {}",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }

                other => Err(format!("ed25519: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
