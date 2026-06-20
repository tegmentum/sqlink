//! Hybrid Public Key Encryption (RFC 9180) for SQL.
//!
//! See Cargo.toml for the function surface and suite naming. The
//! body of this module is mostly a big `match suite { ... }` that
//! plumbs runtime suite selection through hpke's compile-time
//! generics. Each branch instantiates the appropriate `<A, Kdf,
//! Kem>` triple and calls into a generic helper.
//!
//! ## Wire formats
//!
//! Keypair blob:   `priv_bytes || pub_bytes`
//!   X25519:       32 + 32 = 64 bytes
//!   P-256:        32 + 65 = 97 bytes (uncompressed SEC1 pubkey)
//!
//! Sealed blob:    `enc_bytes || ciphertext_with_tag`
//!   X25519:       32-byte ephemeral pubkey then AEAD output
//!   P-256:        65-byte ephemeral pubkey then AEAD output
//!
//! AEAD output is `ciphertext || tag` where tag is 16 bytes for all
//! three supported AEADs (AES-GCM-128/256 and ChaCha20-Poly1305).
//!
//! ## Error policy
//!
//! Any input shape error (unknown suite, wrong-length key, NULL
//! arg, AEAD verification failure, short sealed blob) collapses to
//! SQL NULL. The four AEAD failure modes (wrong key, tampered ct,
//! wrong info, wrong aad) are intentionally indistinguishable --
//! exposing which one failed would leak side-channel info, and the
//! SQL contract callers want is "did this decrypt cleanly, yes or
//! no", not a per-row error.

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

    use hpke::aead::{AesGcm128, AesGcm256, ChaCha20Poly1305};
    use hpke::kdf::HkdfSha256;
    use hpke::kem::{DhP256HkdfSha256, X25519HkdfSha256};
    use hpke::{
        aead::Aead as AeadTrait, kdf::Kdf as KdfTrait, kem::Kem as KemTrait, Deserializable,
        OpModeR, OpModeS, Serializable,
    };

    use rand_core::{CryptoRng, RngCore};

    // ---- Function IDs ----
    const FID_KEYPAIR: u64 = 1;
    const FID_PUB_FROM_PRIV: u64 = 2;
    const FID_SEAL: u64 = 3;
    const FID_OPEN: u64 = 4;
    const FID_VERSION: u64 = 5;

    struct Ext;

    // ---- Suite enumeration ----
    //
    // RFC 9180 identifies a suite by three u16s (KEM, KDF, AEAD).
    // We could parse those numerically, but the `hpke` API needs
    // concrete types at compile time, so we enumerate the
    // supported combinations and match on a Suite enum.

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Suite {
        X25519Sha256ChaChaPoly,
        X25519Sha256Aes128Gcm,
        X25519Sha256Aes256Gcm,
        P256Sha256Aes128Gcm,
        P256Sha256Aes256Gcm,
        P256Sha256ChaChaPoly,
    }

    /// Accept a few aliases per canonical name. Case-insensitive,
    /// whitespace stripped. Empty string defaults to the canonical
    /// X25519+ChaCha suite.
    fn parse_suite(s: &str) -> Option<Suite> {
        let normalized: String = s
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        // Map RFC-style separators ("/", "_", ".") to "-" so
        // "X25519/HKDF-SHA256/ChaCha20Poly1305" matches.
        let normalized: String = normalized
            .chars()
            .map(|c| if matches!(c, '/' | '_' | '.') { '-' } else { c })
            .collect();
        // Drop the "HKDF-" prefix some callers spell out:
        // "X25519-HKDF-SHA256-AES128GCM" -> "X25519-SHA256-AES128GCM".
        let normalized = normalized.replace("HKDF-", "");
        match normalized.as_str() {
            "" | "DEFAULT" | "X25519-SHA256-CHACHA20POLY1305" | "X25519-CHACHA20POLY1305" => {
                Some(Suite::X25519Sha256ChaChaPoly)
            }
            "X25519-SHA256-AES128GCM" | "X25519-AES128GCM" => Some(Suite::X25519Sha256Aes128Gcm),
            "X25519-SHA256-AES256GCM" | "X25519-AES256GCM" => Some(Suite::X25519Sha256Aes256Gcm),
            "P256-SHA256-AES128GCM" | "P256-AES128GCM" => Some(Suite::P256Sha256Aes128Gcm),
            "P256-SHA256-AES256GCM" | "P256-AES256GCM" => Some(Suite::P256Sha256Aes256Gcm),
            "P256-SHA256-CHACHA20POLY1305" | "P256-CHACHA20POLY1305" => {
                Some(Suite::P256Sha256ChaChaPoly)
            }
            _ => None,
        }
    }

    // ---- Arg helpers ----

    fn arg_text(args: &[SqlValue], i: usize) -> Option<String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            Some(SqlValue::Null) => None,
            _ => None,
        }
    }

    fn arg_blob(args: &[SqlValue], i: usize) -> Option<Vec<u8>> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            // NULL or wrong type -> None; the caller turns that into SqlValue::Null.
            _ => None,
        }
    }

    // ---- WASI-backed RNG shim ----
    //
    // hpke's gen_keypair() / encap() want a `RngCore + CryptoRng`.
    // rand_core 0.9 made these traits infallible (panic on
    // upstream error), which is fine because wasi:random/random
    // is by spec always able to satisfy a read. We assert that by
    // panicking if it ever returns an error -- a broken component
    // runtime is the only way that happens.

    struct WasiRng;

    impl RngCore for WasiRng {
        fn next_u32(&mut self) -> u32 {
            let mut b = [0u8; 4];
            self.fill_bytes(&mut b);
            u32::from_le_bytes(b)
        }
        fn next_u64(&mut self) -> u64 {
            let mut b = [0u8; 8];
            self.fill_bytes(&mut b);
            u64::from_le_bytes(b)
        }
        fn fill_bytes(&mut self, dst: &mut [u8]) {
            getrandom::getrandom(dst).expect("wasi:random/random failed");
        }
    }
    impl CryptoRng for WasiRng {}

    // ---- Generic helpers, parameterized over the suite triple ----

    fn gen_keypair_for<Kem: KemTrait>() -> Vec<u8> {
        let mut rng = WasiRng;
        let (sk, pk) = Kem::gen_keypair(&mut rng);
        let sk_bytes = sk.to_bytes();
        let pk_bytes = pk.to_bytes();
        let mut out = Vec::with_capacity(sk_bytes.len() + pk_bytes.len());
        out.extend_from_slice(&sk_bytes);
        out.extend_from_slice(&pk_bytes);
        out
    }

    fn pub_from_priv_for<Kem: KemTrait>(priv_bytes: &[u8]) -> Option<Vec<u8>> {
        let sk = Kem::PrivateKey::from_bytes(priv_bytes).ok()?;
        let pk = Kem::sk_to_pk(&sk);
        Some(pk.to_bytes().to_vec())
    }

    fn seal_for<A: AeadTrait, Kdf: KdfTrait, Kem: KemTrait>(
        pub_bytes: &[u8],
        info: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Option<Vec<u8>> {
        let pk = Kem::PublicKey::from_bytes(pub_bytes).ok()?;
        let mut rng = WasiRng;
        let (encapped_key, ciphertext) = hpke::single_shot_seal::<A, Kdf, Kem, _>(
            &OpModeS::Base,
            &pk,
            info,
            plaintext,
            aad,
            &mut rng,
        )
        .ok()?;
        let enc_bytes = encapped_key.to_bytes();
        let mut out = Vec::with_capacity(enc_bytes.len() + ciphertext.len());
        out.extend_from_slice(&enc_bytes);
        out.extend_from_slice(&ciphertext);
        Some(out)
    }

    fn open_for<A: AeadTrait, Kdf: KdfTrait, Kem: KemTrait>(
        priv_bytes: &[u8],
        info: &[u8],
        aad: &[u8],
        sealed: &[u8],
    ) -> Option<Vec<u8>> {
        // Enc length == serialized pubkey size for DHKEM.
        let nenc = <Kem::EncappedKey as Serializable>::size();
        if sealed.len() < nenc {
            return None;
        }
        let (enc_bytes, ciphertext) = sealed.split_at(nenc);
        let sk = Kem::PrivateKey::from_bytes(priv_bytes).ok()?;
        let encapped_key = Kem::EncappedKey::from_bytes(enc_bytes).ok()?;
        hpke::single_shot_open::<A, Kdf, Kem>(
            &OpModeR::Base,
            &sk,
            &encapped_key,
            info,
            ciphertext,
            aad,
        )
        .ok()
    }

    // ---- Suite dispatch ----
    //
    // The match arms below pin the <Aead, Kdf, Kem> triple for
    // each Suite variant and forward to the generic helpers.

    fn do_keypair(suite: Suite) -> Vec<u8> {
        match suite {
            Suite::X25519Sha256ChaChaPoly
            | Suite::X25519Sha256Aes128Gcm
            | Suite::X25519Sha256Aes256Gcm => gen_keypair_for::<X25519HkdfSha256>(),
            Suite::P256Sha256Aes128Gcm
            | Suite::P256Sha256Aes256Gcm
            | Suite::P256Sha256ChaChaPoly => gen_keypair_for::<DhP256HkdfSha256>(),
        }
    }

    fn do_pub_from_priv(suite: Suite, priv_bytes: &[u8]) -> Option<Vec<u8>> {
        match suite {
            Suite::X25519Sha256ChaChaPoly
            | Suite::X25519Sha256Aes128Gcm
            | Suite::X25519Sha256Aes256Gcm => pub_from_priv_for::<X25519HkdfSha256>(priv_bytes),
            Suite::P256Sha256Aes128Gcm
            | Suite::P256Sha256Aes256Gcm
            | Suite::P256Sha256ChaChaPoly => pub_from_priv_for::<DhP256HkdfSha256>(priv_bytes),
        }
    }

    fn do_seal(
        suite: Suite,
        pub_bytes: &[u8],
        info: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Option<Vec<u8>> {
        match suite {
            Suite::X25519Sha256ChaChaPoly => seal_for::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
                pub_bytes, info, aad, plaintext,
            ),
            Suite::X25519Sha256Aes128Gcm => {
                seal_for::<AesGcm128, HkdfSha256, X25519HkdfSha256>(pub_bytes, info, aad, plaintext)
            }
            Suite::X25519Sha256Aes256Gcm => {
                seal_for::<AesGcm256, HkdfSha256, X25519HkdfSha256>(pub_bytes, info, aad, plaintext)
            }
            Suite::P256Sha256Aes128Gcm => {
                seal_for::<AesGcm128, HkdfSha256, DhP256HkdfSha256>(pub_bytes, info, aad, plaintext)
            }
            Suite::P256Sha256Aes256Gcm => {
                seal_for::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(pub_bytes, info, aad, plaintext)
            }
            Suite::P256Sha256ChaChaPoly => seal_for::<ChaCha20Poly1305, HkdfSha256, DhP256HkdfSha256>(
                pub_bytes, info, aad, plaintext,
            ),
        }
    }

    fn do_open(
        suite: Suite,
        priv_bytes: &[u8],
        info: &[u8],
        aad: &[u8],
        sealed: &[u8],
    ) -> Option<Vec<u8>> {
        match suite {
            Suite::X25519Sha256ChaChaPoly => open_for::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
                priv_bytes, info, aad, sealed,
            ),
            Suite::X25519Sha256Aes128Gcm => {
                open_for::<AesGcm128, HkdfSha256, X25519HkdfSha256>(priv_bytes, info, aad, sealed)
            }
            Suite::X25519Sha256Aes256Gcm => {
                open_for::<AesGcm256, HkdfSha256, X25519HkdfSha256>(priv_bytes, info, aad, sealed)
            }
            Suite::P256Sha256Aes128Gcm => {
                open_for::<AesGcm128, HkdfSha256, DhP256HkdfSha256>(priv_bytes, info, aad, sealed)
            }
            Suite::P256Sha256Aes256Gcm => {
                open_for::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(priv_bytes, info, aad, sealed)
            }
            Suite::P256Sha256ChaChaPoly => open_for::<ChaCha20Poly1305, HkdfSha256, DhP256HkdfSha256>(
                priv_bytes, info, aad, sealed,
            ),
        }
    }

    // ---- SQL entry points ----

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            // Keypair generation and seal both draw from the RNG;
            // SQLite must not fold them across rows.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "hpke".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_KEYPAIR, "hpke_keypair", 1, nd),
                    s(FID_PUB_FROM_PRIV, "hpke_pub_from_priv", 2, det),
                    s(FID_SEAL, "hpke_seal", 5, nd),
                    s(FID_OPEN, "hpke_open", 5, det),
                    s(FID_VERSION, "hpke_version", 0, det),
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
                FID_KEYPAIR => {
                    // suite TEXT (NULL / unknown -> NULL).
                    let Some(suite_s) = arg_text(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(suite) = parse_suite(&suite_s) else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Blob(do_keypair(suite)))
                }
                FID_PUB_FROM_PRIV => {
                    let Some(suite_s) = arg_text(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(suite) = parse_suite(&suite_s) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(priv_b) = arg_blob(&args, 1) else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match do_pub_from_priv(suite, &priv_b) {
                        Some(b) => SqlValue::Blob(b),
                        None => SqlValue::Null,
                    })
                }
                FID_SEAL => {
                    let Some(suite_s) = arg_text(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(suite) = parse_suite(&suite_s) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(pub_b) = arg_blob(&args, 1) else {
                        return Ok(SqlValue::Null);
                    };
                    // info / aad / plaintext: NULL coerces to empty
                    // bytes. That matches the "no info" / "no aad" /
                    // "encrypt nothing" semantics the RFC defines.
                    let info = arg_blob(&args, 2).unwrap_or_default();
                    let aad = arg_blob(&args, 3).unwrap_or_default();
                    let pt = arg_blob(&args, 4).unwrap_or_default();
                    Ok(match do_seal(suite, &pub_b, &info, &aad, &pt) {
                        Some(b) => SqlValue::Blob(b),
                        None => SqlValue::Null,
                    })
                }
                FID_OPEN => {
                    let Some(suite_s) = arg_text(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(suite) = parse_suite(&suite_s) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(priv_b) = arg_blob(&args, 1) else {
                        return Ok(SqlValue::Null);
                    };
                    let info = arg_blob(&args, 2).unwrap_or_default();
                    let aad = arg_blob(&args, 3).unwrap_or_default();
                    let Some(sealed) = arg_blob(&args, 4) else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match do_open(suite, &priv_b, &info, &aad, &sealed) {
                        Some(b) => SqlValue::Blob(b),
                        None => SqlValue::Null,
                    })
                }
                FID_VERSION => {
                    let v = format!("hpke 0.13 (RFC 9180); extension {}", env!("CARGO_PKG_VERSION"));
                    Ok(SqlValue::Text(v))
                }
                other => Err(format!("hpke: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
