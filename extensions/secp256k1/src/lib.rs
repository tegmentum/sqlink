//! secp256k1 ECDSA + signature recovery for SQL.
//!
//! Pairs with the `jwt` extension (HMAC + Ed25519) and `tls-cert`
//! (nistp / RSA via x509-parser). The single differentiating
//! feature secp256k1 carries over the NIST P-curves: *signature
//! recovery*  given a (message, signature_with_recid) pair you
//! can recover the signer's compressed public key. Ethereum's
//! transaction format depends on this (the "from" address is
//! never serialized; it's recovered from the signature).
//!
//! Function surface (PLAN-more-extensions-5.md  1):
//!
//!   secp256k1_keypair()                          -> blob   (65 bytes: 32 priv || 33 compressed pub)
//!   secp256k1_pub_from_priv(priv_blob)           -> blob   (33-byte compressed)
//!   secp256k1_pub_uncompressed(pub_blob)         -> blob   (65-byte uncompressed)
//!   secp256k1_sign(priv_blob, message)           -> blob   (64-byte compact r||s)
//!   secp256k1_sign_recoverable(priv_blob, msg)   -> blob   (65-byte r||s||recid)
//!   secp256k1_verify(pub_blob, msg, sig_blob)    -> integer 0/1
//!   secp256k1_recover(msg, sig_recoverable)      -> blob   (33-byte compressed pub)
//!   secp256k1_eth_address(pub_blob)              -> text   (0x-prefixed, last 20 bytes of keccak256(uncompressed[1..]))
//!   secp256k1_btc_address_p2pkh(pub_blob)        -> text   (base58check P2PKH)
//!   secp256k1_version()                          -> text
//!
//! `message` is a *pre-computed* 32-byte hash blob. The extension
//! does NOT hash for callers  Ethereum uses keccak256, Bitcoin
//! uses double-SHA-256, JWS uses SHA-256; callers pick the right
//! one.
//!
//! NULL  NULL on every fn. Wrong-length / malformed keys +
//! signatures  NULL (not an error). The sign* functions can
//! also surface NULL if the deterministic-k RFC 6979 path
//! exhausts (vanishingly improbable).
//!
//! Profile note: k256 0.13 in `precomputed-tables` mode trades
//! ~50 KiB of binary size for a ~4x speedup on scalar mul. We
//! pick speed -- the SQL surface is variadic enough that callers
//! will issue many sign / verify per query.

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

    use k256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
    use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
    use k256::elliptic_curve::sec1::ToEncodedPoint;
    use k256::PublicKey;

    // The `Digest` trait must be in scope wherever `.digest()` or
    // `.new()` is called for one of these hashers; the use-as
    // alias avoids name collisions between the three crates.
    use ripemd::{Digest as _, Ripemd160};
    use sha2::{Digest as _, Sha256};
    use sha3::{Digest as _, Keccak256};

    const FID_KEYPAIR: u64 = 1;
    const FID_PUB_FROM_PRIV: u64 = 2;
    const FID_PUB_UNCOMPRESSED: u64 = 3;
    const FID_SIGN: u64 = 4;
    const FID_SIGN_RECOVERABLE: u64 = 5;
    const FID_VERIFY: u64 = 6;
    const FID_RECOVER: u64 = 7;
    const FID_ETH_ADDRESS: u64 = 8;
    const FID_BTC_P2PKH: u64 = 9;
    const FID_VERSION: u64 = 10;

    struct Ext;

    /// Optional-blob: NULL passes through as None; BLOB / TEXT
    /// surface as bytes; INTEGER / REAL reject (no sensible
    /// coercion for binary keys / signatures).
    fn opt_blob(args: &[SqlValue], i: usize) -> Result<Option<Vec<u8>>, String> {
        match args.get(i) {
            None => Err(format!("missing blob arg at {i}")),
            Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Blob(b)) => Ok(Some(b.clone())),
            Some(SqlValue::Text(s)) => Ok(Some(s.as_bytes().to_vec())),
            _ => Err(format!("arg at {i} must be BLOB / TEXT / NULL")),
        }
    }

    /// Wrong-shape input  NULL result. Used to keep the surface
    /// SQL-friendly: a junky key shouldn't make the whole query
    /// throw, just yield NULL for that row.
    fn nullify<T>(r: Result<T, String>) -> Option<T> {
        r.ok()
    }

    /// Parse a 32-byte private key blob into a SigningKey. Wrong
    /// length / out-of-range scalar  None (NULL).
    fn parse_priv(bytes: &[u8]) -> Option<SigningKey> {
        if bytes.len() != 32 {
            return None;
        }
        SigningKey::from_bytes(bytes.into()).ok()
    }

    /// Parse a public key blob (SEC1 compressed 33 bytes or
    /// uncompressed 65 bytes). Wrong length / not on curve  None.
    fn parse_pub(bytes: &[u8]) -> Option<PublicKey> {
        // PublicKey::from_sec1_bytes handles both 33 and 65 byte
        // SEC1 encodings and rejects off-curve / identity points.
        PublicKey::from_sec1_bytes(bytes).ok()
    }

    /// Parse a 32-byte message hash blob. The functions take a
    /// pre-computed digest; SQL caller does the hashing.
    fn parse_msg(bytes: &[u8]) -> Option<[u8; 32]> {
        let arr: [u8; 32] = bytes.try_into().ok()?;
        Some(arr)
    }

    /// Compressed (33-byte) SEC1 encoding of a public key.
    fn pub_compressed(pk: &PublicKey) -> Vec<u8> {
        pk.to_encoded_point(true).as_bytes().to_vec()
    }

    /// Uncompressed (65-byte) SEC1 encoding: 0x04 || x || y.
    fn pub_uncompressed(pk: &PublicKey) -> Vec<u8> {
        pk.to_encoded_point(false).as_bytes().to_vec()
    }

    // --- keypair generation -------------------------------------

    /// Generate a fresh keypair: 32-byte private || 33-byte
    /// compressed public = 65 bytes. The PLAN text said "64
    /// bytes" but the components it listed sum to 65; we follow
    /// the components since they're the concrete contract.
    fn gen_keypair() -> Result<Vec<u8>, String> {
        // Randomness via getrandom -> wasi:random/random.
        // SigningKey::random expects an `OsRng`-like type that
        // impls rand_core::CryptoRngCore; we can roll a minimal
        // wrapper here, but the simpler path is generate 32 bytes
        // and try-from until valid (rejection sample on the
        // ~2^-128 chance the scalar is 0 or >= n).
        for _ in 0..16 {
            let mut seed = [0u8; 32];
            getrandom::getrandom(&mut seed)
                .map_err(|e| format!("secp256k1_keypair: rng: {e}"))?;
            if let Ok(sk) = SigningKey::from_bytes(&seed.into()) {
                let pk = sk.verifying_key().to_encoded_point(true);
                let mut out = Vec::with_capacity(32 + 33);
                out.extend_from_slice(&seed);
                out.extend_from_slice(pk.as_bytes());
                return Ok(out);
            }
        }
        // Probability of 16 consecutive invalid scalars: ~2^-2048.
        // If we land here something is wrong with rng.
        Err("secp256k1_keypair: rng produced no valid scalar".into())
    }

    // --- ETH address --------------------------------------------

    /// 0x-prefixed lowercase 40-hex Ethereum address: last 20
    /// bytes of keccak256(uncompressed_pub[1..]) -- i.e. drop the
    /// 0x04 SEC1 prefix, hash the 64-byte (x || y) form.
    fn eth_address(pk: &PublicKey) -> String {
        let unc = pub_uncompressed(pk);
        // unc[0] is 0x04 (SEC1 uncompressed prefix). Hash the
        // remaining 64 bytes.
        let mut h = Keccak256::new();
        h.update(&unc[1..]);
        let out = h.finalize();
        let mut addr = String::with_capacity(2 + 40);
        addr.push_str("0x");
        addr.push_str(&hex::encode(&out[12..32]));
        addr
    }

    // --- BTC P2PKH ----------------------------------------------

    /// Bitcoin P2PKH (mainnet, version byte 0x00) base58check
    /// address of a public key. Compressed pubkey is the modern
    /// convention; SEC1-compressed is what the address is
    /// computed against.
    fn btc_p2pkh(pk: &PublicKey) -> String {
        use base58::ToBase58;
        let pk_bytes = pub_compressed(pk);
        // HASH160 = RIPEMD-160(SHA-256(pk))
        let sha = Sha256::digest(&pk_bytes);
        let ripemd: [u8; 20] = Ripemd160::digest(&sha).into();
        // Payload: version byte (0x00 mainnet) || 20-byte hash
        let mut payload = Vec::with_capacity(1 + 20 + 4);
        payload.push(0x00);
        payload.extend_from_slice(&ripemd);
        // Checksum = first 4 bytes of double-SHA-256(payload)
        let c1 = Sha256::digest(&payload);
        let c2 = Sha256::digest(&c1);
        payload.extend_from_slice(&c2[..4]);
        payload.to_base58()
    }

    // --- signing ------------------------------------------------

    /// 64-byte compact r||s ECDSA signature over a 32-byte
    /// pre-computed message hash. RFC 6979 deterministic k, so
    /// the output is bit-exact reproducible for a given priv+msg
    /// pair. Outputs are *low-s* normalized (BIP-62 / Bitcoin
    /// convention): if s > n/2, replace with n-s. k256 handles
    /// this by default via Signature::normalize_s.
    fn sign_compact(sk: &SigningKey, msg: &[u8; 32]) -> Option<Vec<u8>> {
        let sig: Signature = sk.sign_prehash(msg).ok()?;
        // sign_prehash already produces low-s on k256 0.13.
        // Compact: r (32) || s (32).
        Some(sig.to_bytes().to_vec())
    }

    /// 65-byte recoverable signature: r||s||recid. recid is 0 or
    /// 1 (k256 normalizes to the canonical low-s pair, so the
    /// "high" bit of recid never occurs).
    fn sign_recoverable(sk: &SigningKey, msg: &[u8; 32]) -> Option<Vec<u8>> {
        let (sig, recid) = sk.sign_prehash_recoverable(msg).ok()?;
        let mut out = Vec::with_capacity(65);
        out.extend_from_slice(&sig.to_bytes());
        out.push(recid.to_byte());
        Some(out)
    }

    fn verify(pk: &PublicKey, msg: &[u8; 32], sig_bytes: &[u8]) -> bool {
        if sig_bytes.len() != 64 {
            return false;
        }
        let sig = match Signature::try_from(sig_bytes) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let vk: VerifyingKey = pk.into();
        vk.verify_prehash(msg, &sig).is_ok()
    }

    fn recover(msg: &[u8; 32], sig_recoverable: &[u8]) -> Option<Vec<u8>> {
        if sig_recoverable.len() != 65 {
            return None;
        }
        let sig = Signature::try_from(&sig_recoverable[..64]).ok()?;
        let recid = RecoveryId::try_from(sig_recoverable[64]).ok()?;
        let vk = VerifyingKey::recover_from_prehash(msg, &sig, recid).ok()?;
        // VerifyingKey -> compressed SEC1
        Some(vk.to_encoded_point(true).as_bytes().to_vec())
    }

    // --- guest impl ---------------------------------------------

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
                name: "secp256k1".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_KEYPAIR, "secp256k1_keypair", 0, nd),
                    s(FID_PUB_FROM_PRIV, "secp256k1_pub_from_priv", 1, det),
                    s(FID_PUB_UNCOMPRESSED, "secp256k1_pub_uncompressed", 1, det),
                    s(FID_SIGN, "secp256k1_sign", 2, det),
                    s(FID_SIGN_RECOVERABLE, "secp256k1_sign_recoverable", 2, det),
                    s(FID_VERIFY, "secp256k1_verify", 3, det),
                    s(FID_RECOVER, "secp256k1_recover", 2, det),
                    s(FID_ETH_ADDRESS, "secp256k1_eth_address", 1, det),
                    s(FID_BTC_P2PKH, "secp256k1_btc_address_p2pkh", 1, det),
                    s(FID_VERSION, "secp256k1_version", 0, det),
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
                preferred_prefix: Some("secp256k1".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.secp256k1".into()),
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
                        Some(sk) => {
                            let pk: PublicKey = sk.verifying_key().into();
                            SqlValue::Blob(pub_compressed(&pk))
                        }
                        None => SqlValue::Null,
                    })
                }

                FID_PUB_UNCOMPRESSED => {
                    let Some(b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match parse_pub(&b) {
                        Some(pk) => SqlValue::Blob(pub_uncompressed(&pk)),
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
                    let Some(msg) = parse_msg(&msg_b) else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match sign_compact(&sk, &msg) {
                        Some(s) => SqlValue::Blob(s),
                        None => SqlValue::Null,
                    })
                }

                FID_SIGN_RECOVERABLE => {
                    let Some(priv_b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(msg_b) = opt_blob(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(sk) = parse_priv(&priv_b) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(msg) = parse_msg(&msg_b) else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match sign_recoverable(&sk, &msg) {
                        Some(s) => SqlValue::Blob(s),
                        None => SqlValue::Null,
                    })
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
                    let Some(msg) = parse_msg(&msg_b) else {
                        return Ok(SqlValue::Integer(0));
                    };
                    Ok(SqlValue::Integer(verify(&pk, &msg, &sig_b) as i64))
                }

                FID_RECOVER => {
                    let Some(msg_b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(sig_b) = opt_blob(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(msg) = parse_msg(&msg_b) else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match recover(&msg, &sig_b) {
                        Some(p) => SqlValue::Blob(p),
                        None => SqlValue::Null,
                    })
                }

                FID_ETH_ADDRESS => {
                    let Some(b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match parse_pub(&b) {
                        Some(pk) => SqlValue::Text(eth_address(&pk)),
                        None => SqlValue::Null,
                    })
                }

                FID_BTC_P2PKH => {
                    let Some(b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match parse_pub(&b) {
                        Some(pk) => SqlValue::Text(btc_p2pkh(&pk)),
                        None => SqlValue::Null,
                    })
                }

                FID_VERSION => {
                    let v = format!(
                        "k256 0.13; extension {}",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }

                other => Err(format!("secp256k1: unknown func id {other}")),
            }
        }
    }

    // Silence unused-import / dead-code warnings for helpers that
    // only exist for the typed wrapper boundary. (`nullify` is a
    // documented helper used in earlier drafts; kept for symmetry
    // with future error-propagation surfaces.)
    #[allow(dead_code)]
    fn _keep(_: fn(Result<(), String>) -> Option<()>) {}
    #[allow(dead_code)]
    fn _touch() {
        _keep(nullify);
    }

    bindings::export!(Ext with_types_in bindings);
}
