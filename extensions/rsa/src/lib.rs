//! RSA signing, verification, encryption and decryption for SQL.
//!
//! Closes the gap the `jwt` extension left when it deferred RS256
//! (PKCS#1 v1.5 over SHA-256) to v2. Pairs with `ed25519` (small
//! deterministic signatures) + `secp256k1` (Bitcoin/Ethereum curve)
//! as the third pillar of pure-rust public-key crypto in this catalog.
//!
//! Function surface:
//!
//!   rsa_generate(bits)                       -> text   JSON {priv_pem, pub_pem}
//!   rsa_pub_from_priv(priv_pem)              -> text   SPKI public PEM
//!   rsa_sign_pkcs1v15(priv_pem, msg)         -> blob   PKCS#1 v1.5 sig (SHA-256)
//!   rsa_verify_pkcs1v15(pub_pem, msg, sig)   -> integer 0/1
//!   rsa_sign_pss(priv_pem, msg)              -> blob   PSS sig (SHA-256)
//!   rsa_verify_pss(pub_pem, msg, sig)        -> integer 0/1
//!   rsa_encrypt_oaep(pub_pem, pt)            -> blob   OAEP (SHA-256 mgf1)
//!   rsa_decrypt_oaep(priv_pem, ct)           -> blob | NULL
//!   rsa_version()                            -> text
//!
//! Conventions:
//!   * Signature schemes hash the message inside the extension with
//!     SHA-256; callers pass plaintext. (Different from
//!     `secp256k1_sign`, which takes a pre-computed 32-byte digest.)
//!   * Private keys are PKCS#8 PEM on output, PKCS#8 or PKCS#1 on input.
//!   * Public keys are SPKI PEM on output, SPKI or PKCS#1 on input.
//!   * Verify  0 on any failure (malformed key, wrong sig, tampered msg).
//!   * `rsa_decrypt_oaep`  NULL on any decrypt failure  matches the
//!     `aead` crate's "cleanly decrypted yes/no" SQL contract.
//!   * `rsa_generate(bits)` floors bits at 2048; smaller sizes are
//!     refused. 3072 / 4096 work but key generation is slow on wasm
//!     (multiple seconds at 4096).
//!
//! Randomness flows from `getrandom`'s WASI path  the WASI reactor
//! adapter routes `getrandom` to `wasi:random/random`. We seed a
//! `rand_chacha::ChaCha20Rng` per call (the rsa crate's documented
//! example RNG) rather than passing `OsRng` directly, since the rsa
//! crate's bounds want `CryptoRngCore`.

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

    use rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey};
    use rsa::pkcs1v15::{
        Signature as Pkcs1v15Sig, SigningKey as Pkcs1v15Signing,
        VerifyingKey as Pkcs1v15Verifying,
    };
    use rsa::pkcs8::{
        DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding,
    };
    use rsa::pss::{
        BlindedSigningKey as PssSigning, Signature as PssSig, VerifyingKey as PssVerifying,
    };
    use rsa::signature::{RandomizedSigner, SignatureEncoding, Verifier};
    use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};

    use rand_chacha::ChaCha20Rng;
    use rand_core::SeedableRng;
    use sha2::Sha256;

    const FID_GENERATE: u64 = 1;
    const FID_PUB_FROM_PRIV: u64 = 2;
    const FID_SIGN_PKCS1V15: u64 = 3;
    const FID_VERIFY_PKCS1V15: u64 = 4;
    const FID_SIGN_PSS: u64 = 5;
    const FID_VERIFY_PSS: u64 = 6;
    const FID_ENCRYPT_OAEP: u64 = 7;
    const FID_DECRYPT_OAEP: u64 = 8;
    const FID_VERSION: u64 = 9;

    struct Ext;

    // --- arg helpers --------------------------------------------

    /// PEM keys arrive as TEXT (the natural form, since PEM is
    /// base64+armor). BLOB also accepted in case a caller staged the
    /// PEM as raw bytes. NULL propagates as None and the caller
    /// returns NULL  matches the "junky input  NULL, not error" SQL
    /// contract.
    fn opt_text(args: &[SqlValue], i: usize) -> Result<Option<String>, String> {
        match args.get(i) {
            None => Err(format!("missing TEXT arg at {i}")),
            Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(SqlValue::Blob(b)) => match core::str::from_utf8(b) {
                Ok(s) => Ok(Some(s.to_string())),
                Err(_) => Err(format!("arg at {i} BLOB is not valid UTF-8 PEM")),
            },
            _ => Err(format!("arg at {i} must be TEXT / BLOB / NULL")),
        }
    }

    /// Raw bytes (message plaintext or signature). TEXT accepted for
    /// convenience  the bytes are the UTF-8 encoding of the text.
    fn opt_bytes(args: &[SqlValue], i: usize) -> Result<Option<Vec<u8>>, String> {
        match args.get(i) {
            None => Err(format!("missing BLOB arg at {i}")),
            Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Blob(b)) => Ok(Some(b.clone())),
            Some(SqlValue::Text(s)) => Ok(Some(s.as_bytes().to_vec())),
            _ => Err(format!("arg at {i} must be BLOB / TEXT / NULL")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    // --- RNG ----------------------------------------------------

    /// Build a fresh ChaCha20Rng seeded from the host CSPRNG.
    /// ChaCha20Rng impls `CryptoRngCore`, the trait every rsa-crate
    /// API that takes randomness is bounded by.
    fn rng(fname: &str) -> Result<ChaCha20Rng, String> {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).map_err(|e| format!("{fname}: rng: {e}"))?;
        Ok(ChaCha20Rng::from_seed(seed))
    }

    // --- PEM parsers (lenient: accept PKCS#8 or PKCS#1) ---------

    /// Try PKCS#8 first (BEGIN PRIVATE KEY), fall back to PKCS#1
    /// (BEGIN RSA PRIVATE KEY). None on any failure.
    fn parse_priv(pem: &str) -> Option<RsaPrivateKey> {
        if let Ok(k) = RsaPrivateKey::from_pkcs8_pem(pem) {
            return Some(k);
        }
        RsaPrivateKey::from_pkcs1_pem(pem).ok()
    }

    /// Try SPKI first (BEGIN PUBLIC KEY), fall back to PKCS#1
    /// (BEGIN RSA PUBLIC KEY). None on any failure.
    fn parse_pub(pem: &str) -> Option<RsaPublicKey> {
        if let Ok(k) = RsaPublicKey::from_public_key_pem(pem) {
            return Some(k);
        }
        RsaPublicKey::from_pkcs1_pem(pem).ok()
    }

    // --- generate ----------------------------------------------

    /// Generate an RSA keypair, emit JSON `{"priv_pem": "...",
    /// "pub_pem": "..."}`. PEM newlines escape to `\n` for JSON-safe
    /// transport. Bits clamped to >= 2048; smaller sizes refused.
    fn rsa_generate(bits: i64) -> Result<String, String> {
        if bits < 2048 {
            return Err(format!(
                "rsa_generate: bits must be >= 2048 (got {bits}); \
                 smaller keys are insecure"
            ));
        }
        if bits > 8192 {
            return Err(format!(
                "rsa_generate: bits must be <= 8192 (got {bits}); \
                 sizes above 4096 are impractically slow on wasm"
            ));
        }
        let bits_usize = bits as usize;
        let mut r = rng("rsa_generate")?;
        let priv_key = RsaPrivateKey::new(&mut r, bits_usize)
            .map_err(|e| format!("rsa_generate: keygen: {e}"))?;
        let pub_key = RsaPublicKey::from(&priv_key);
        let priv_pem = priv_key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|e| format!("rsa_generate: priv pem: {e}"))?
            .to_string();
        let pub_pem = pub_key
            .to_public_key_pem(LineEnding::LF)
            .map_err(|e| format!("rsa_generate: pub pem: {e}"))?;
        let priv_json = json_escape(&priv_pem);
        let pub_json = json_escape(&pub_pem);
        Ok(format!(
            "{{\"priv_pem\":\"{priv_json}\",\"pub_pem\":\"{pub_json}\"}}"
        ))
    }

    /// Minimal JSON string escaper. PEM only contains printable ASCII
    /// + `\n`, so the slow path (`\u00xx`) is reachable only on
    /// adversarial input  no performance concern.
    fn json_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 16);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out
    }

    // --- pub_from_priv ------------------------------------------

    fn pub_from_priv(priv_pem: &str) -> Option<String> {
        let priv_key = parse_priv(priv_pem)?;
        let pub_key = RsaPublicKey::from(&priv_key);
        pub_key.to_public_key_pem(LineEnding::LF).ok()
    }

    // --- signature schemes --------------------------------------

    fn sign_pkcs1v15(priv_pem: &str, msg: &[u8]) -> Option<Vec<u8>> {
        let priv_key = parse_priv(priv_pem)?;
        let signing_key: Pkcs1v15Signing<Sha256> = Pkcs1v15Signing::new(priv_key);
        let mut r = rng("rsa_sign_pkcs1v15").ok()?;
        // PKCS#1 v1.5 is technically deterministic but the
        // RandomizedSigner trait wants an RNG; the rsa crate ignores
        // it for the v1.5 path.
        let sig: Pkcs1v15Sig = signing_key.sign_with_rng(&mut r, msg);
        Some(sig.to_bytes().to_vec())
    }

    fn verify_pkcs1v15(pub_pem: &str, msg: &[u8], sig_bytes: &[u8]) -> bool {
        let Some(pub_key) = parse_pub(pub_pem) else {
            return false;
        };
        let verifying_key: Pkcs1v15Verifying<Sha256> = Pkcs1v15Verifying::new(pub_key);
        let Ok(sig) = Pkcs1v15Sig::try_from(sig_bytes) else {
            return false;
        };
        verifying_key.verify(msg, &sig).is_ok()
    }

    fn sign_pss(priv_pem: &str, msg: &[u8]) -> Option<Vec<u8>> {
        let priv_key = parse_priv(priv_pem)?;
        let signing_key: PssSigning<Sha256> = PssSigning::new(priv_key);
        let mut r = rng("rsa_sign_pss").ok()?;
        let sig: PssSig = signing_key.sign_with_rng(&mut r, msg);
        Some(sig.to_bytes().to_vec())
    }

    fn verify_pss(pub_pem: &str, msg: &[u8], sig_bytes: &[u8]) -> bool {
        let Some(pub_key) = parse_pub(pub_pem) else {
            return false;
        };
        let verifying_key: PssVerifying<Sha256> = PssVerifying::new(pub_key);
        let Ok(sig) = PssSig::try_from(sig_bytes) else {
            return false;
        };
        verifying_key.verify(msg, &sig).is_ok()
    }

    // --- OAEP ---------------------------------------------------

    fn encrypt_oaep(pub_pem: &str, pt: &[u8]) -> Result<Vec<u8>, String> {
        let pub_key =
            parse_pub(pub_pem).ok_or_else(|| "rsa_encrypt_oaep: bad public key".to_string())?;
        let mut r = rng("rsa_encrypt_oaep")?;
        let padding = Oaep::new::<Sha256>();
        pub_key
            .encrypt(&mut r, padding, pt)
            .map_err(|e| format!("rsa_encrypt_oaep: {e}"))
    }

    /// None on any decrypt failure. The failure modes (wrong key,
    /// tampered ciphertext, bad OAEP padding) are indistinguishable
    /// by design  exposing which one failed would surface a
    /// padding-oracle channel.
    fn decrypt_oaep(priv_pem: &str, ct: &[u8]) -> Option<Vec<u8>> {
        let priv_key = parse_priv(priv_pem)?;
        let padding = Oaep::new::<Sha256>();
        priv_key.decrypt(padding, ct).ok()
    }

    // --- guest impl ---------------------------------------------

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            // Anything that pulls fresh randomness (PSS / OAEP / keygen)
            // must NOT be folded across rows.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "rsa".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_GENERATE, "rsa_generate", 1, nd),
                    s(FID_PUB_FROM_PRIV, "rsa_pub_from_priv", 1, det),
                    s(FID_SIGN_PKCS1V15, "rsa_sign_pkcs1v15", 2, det),
                    s(FID_VERIFY_PKCS1V15, "rsa_verify_pkcs1v15", 3, det),
                    s(FID_SIGN_PSS, "rsa_sign_pss", 2, nd),
                    s(FID_VERIFY_PSS, "rsa_verify_pss", 3, det),
                    s(FID_ENCRYPT_OAEP, "rsa_encrypt_oaep", 2, nd),
                    s(FID_DECRYPT_OAEP, "rsa_decrypt_oaep", 2, det),
                    s(FID_VERSION, "rsa_version", 0, det),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_GENERATE => {
                    let bits = arg_int(&args, 0, "rsa_generate")?;
                    Ok(SqlValue::Text(rsa_generate(bits)?))
                }

                FID_PUB_FROM_PRIV => {
                    let Some(pem) = opt_text(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match pub_from_priv(&pem) {
                        Some(s) => SqlValue::Text(s),
                        None => SqlValue::Null,
                    })
                }

                FID_SIGN_PKCS1V15 => {
                    let Some(pem) = opt_text(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(msg) = opt_bytes(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match sign_pkcs1v15(&pem, &msg) {
                        Some(b) => SqlValue::Blob(b),
                        None => SqlValue::Null,
                    })
                }

                FID_VERIFY_PKCS1V15 => {
                    let Some(pem) = opt_text(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(msg) = opt_bytes(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(sig) = opt_bytes(&args, 2)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Integer(verify_pkcs1v15(&pem, &msg, &sig) as i64))
                }

                FID_SIGN_PSS => {
                    let Some(pem) = opt_text(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(msg) = opt_bytes(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match sign_pss(&pem, &msg) {
                        Some(b) => SqlValue::Blob(b),
                        None => SqlValue::Null,
                    })
                }

                FID_VERIFY_PSS => {
                    let Some(pem) = opt_text(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(msg) = opt_bytes(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(sig) = opt_bytes(&args, 2)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Integer(verify_pss(&pem, &msg, &sig) as i64))
                }

                FID_ENCRYPT_OAEP => {
                    let Some(pem) = opt_text(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(pt) = opt_bytes(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Blob(encrypt_oaep(&pem, &pt)?))
                }

                FID_DECRYPT_OAEP => {
                    let Some(pem) = opt_text(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(ct) = opt_bytes(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match decrypt_oaep(&pem, &ct) {
                        Some(b) => SqlValue::Blob(b),
                        None => SqlValue::Null,
                    })
                }

                FID_VERSION => {
                    let v = format!("rsa 0.9; extension {}", env!("CARGO_PKG_VERSION"));
                    Ok(SqlValue::Text(v))
                }

                other => Err(format!("rsa: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
