//! AES non-AEAD modes for SQL: CBC, CTR, and SIV.
//!
//! Pairs with the `aead` crate (AES-GCM + ChaCha20-Poly1305). The
//! three modes here cover the rest of the AES surface most callers
//! actually meet at boundaries:
//!
//!   CBC  PKCS#7 padded; 128/192/256-bit key; 16-byte IV. Legacy
//!         but still the on-the-wire mode in TLS 1.0/1.1, IPsec,
//!         most disk-encryption tooling, and shrink-wrapped DBs.
//!   CTR  RFC 3686 layout: 12-byte nonce, 32-bit big-endian counter
//!         starting at 1. Symmetric  decrypt == encrypt.
//!   SIV  RFC 5297 deterministic AEAD. 32-byte key  AES-128-SIV,
//!         64-byte key  AES-256-SIV. The S2V tag is prepended.
//!
//! Failure-mode contract follows `aead`: decrypt returns NULL on bad
//! padding (CBC) or bad tag (SIV) rather than raising. CTR has no
//! integrity tag so its decrypt is just a re-keystream.
//!
//! Test vectors locked in smoke.sql:
//!   AES-CTR  RFC 3686 §6 vector #1
//!   AES-CBC  NIST SP 800-38A §F.2.1
//!   AES-SIV  RFC 5297 §A.1

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use aes::{Aes128, Aes192, Aes256};
    use aes::cipher::{
        BlockDecryptMut, BlockEncryptMut, KeyIvInit, StreamCipher,
        block_padding::Pkcs7,
    };
    // Deterministic SIV  the `Aes128Siv` / `Aes256Siv` types expose
    // RFC 5297's worked example surface directly: `encrypt(headers,
    // plaintext)` where `headers` is the iterator of AD components.
    // The sibling `SivAead` type treats its 16-byte nonce as the
    // final AD component, which is fine for AEAD users but means
    // round-tripping RFC test vectors goes via the lower-level Siv.
    use aes_siv::siv::{Aes128Siv, Aes256Siv};
    use aes_siv::aead::generic_array::GenericArray;
    use aes_siv::KeyInit;

    // CBC encryptor/decryptor type aliases. `cbc::Encryptor<C>` is
    // generic over the block cipher  we instantiate three of each so
    // the call sites only branch on key length, not on cipher type.
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;
    type Aes192CbcEnc = cbc::Encryptor<Aes192>;
    type Aes256CbcEnc = cbc::Encryptor<Aes256>;
    type Aes128CbcDec = cbc::Decryptor<Aes128>;
    type Aes192CbcDec = cbc::Decryptor<Aes192>;
    type Aes256CbcDec = cbc::Decryptor<Aes256>;

    // CTR uses a 128-bit counter type by default. We use Ctr32BE so
    // the increment width matches RFC 3686's 32-bit counter; that's
    // what AES-GCM uses internally too.
    type Aes128Ctr32BE = ctr::Ctr32BE<Aes128>;
    type Aes192Ctr32BE = ctr::Ctr32BE<Aes192>;
    type Aes256Ctr32BE = ctr::Ctr32BE<Aes256>;

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
    const FID_CBC_ENC: u64 = 1;
    const FID_CBC_DEC: u64 = 2;
    const FID_CTR_ENC: u64 = 3;
    const FID_CTR_DEC: u64 = 4;
    const FID_SIV_ENC: u64 = 5;
    const FID_SIV_DEC: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

    // ---- Arg helpers ----

    /// Accept BLOB or TEXT. Same rule as the `aead` crate: keys /
    /// nonces / ciphertexts are most natural as BLOB, but TEXT
    /// passphrases / plaintext are convenient. INTEGER / REAL / NULL
    /// reject.
    fn arg_bytes(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            Some(SqlValue::Null) => Err(format!("{fname}: arg {i} is NULL")),
            _ => Err(format!("{fname}: arg {i} must be BLOB or TEXT")),
        }
    }

    /// Optional AAD slot for SIV (3rd argument, index 2). Missing or
    /// NULL  empty AAD, matching standard "no AAD" semantics.
    fn opt_aad(args: &[SqlValue], idx: usize) -> Vec<u8> {
        match args.get(idx) {
            None | Some(SqlValue::Null) => Vec::new(),
            Some(SqlValue::Blob(b)) => b.clone(),
            Some(SqlValue::Text(s)) => s.as_bytes().to_vec(),
            Some(SqlValue::Integer(n)) => n.to_string().into_bytes(),
            Some(SqlValue::Real(r)) => r.to_string().into_bytes(),
        }
    }

    fn check_iv(iv: &[u8], fname: &str) -> Result<[u8; 16], String> {
        iv.try_into()
            .map_err(|_| format!("{fname}: IV must be 16 bytes, got {}", iv.len()))
    }

    /// CTR nonce per RFC 3686: 12 bytes. The 32-bit counter starts at
    /// 1 (the cipher seeds it for us  Ctr32BE does this implicitly
    /// once we pad the nonce out to 16 bytes with 00000001).
    fn ctr_iv_from_nonce(nonce: &[u8], fname: &str) -> Result<[u8; 16], String> {
        if nonce.len() != 12 {
            return Err(format!("{fname}: nonce must be 12 bytes, got {}", nonce.len()));
        }
        let mut iv = [0u8; 16];
        iv[..12].copy_from_slice(nonce);
        // Counter block = nonce || 00 00 00 01.
        iv[15] = 1;
        Ok(iv)
    }

    // ---- CBC ----

    fn cbc_encrypt(key: &[u8], iv: &[u8; 16], pt: &[u8]) -> Result<Vec<u8>, String> {
        match key.len() {
            16 => Ok(Aes128CbcEnc::new(key.into(), iv.into())
                .encrypt_padded_vec_mut::<Pkcs7>(pt)),
            24 => Ok(Aes192CbcEnc::new(key.into(), iv.into())
                .encrypt_padded_vec_mut::<Pkcs7>(pt)),
            32 => Ok(Aes256CbcEnc::new(key.into(), iv.into())
                .encrypt_padded_vec_mut::<Pkcs7>(pt)),
            n => Err(format!("aes_cbc_encrypt: key must be 16/24/32 bytes, got {n}")),
        }
    }

    fn cbc_decrypt(key: &[u8], iv: &[u8; 16], ct: &[u8]) -> Option<Vec<u8>> {
        match key.len() {
            16 => Aes128CbcDec::new(key.into(), iv.into())
                .decrypt_padded_vec_mut::<Pkcs7>(ct).ok(),
            24 => Aes192CbcDec::new(key.into(), iv.into())
                .decrypt_padded_vec_mut::<Pkcs7>(ct).ok(),
            32 => Aes256CbcDec::new(key.into(), iv.into())
                .decrypt_padded_vec_mut::<Pkcs7>(ct).ok(),
            _ => None,
        }
    }

    // ---- CTR ----

    /// One function body since CTR is symmetric: keystream the buffer
    /// in place. Caller is responsible for never reusing (key, nonce)
    /// pairs  there is no auth tag to detect misuse here.
    fn ctr_crypt(key: &[u8], iv: &[u8; 16], buf: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = buf.to_vec();
        match key.len() {
            16 => {
                let mut c = Aes128Ctr32BE::new(key.into(), iv.into());
                c.apply_keystream(&mut out);
            }
            24 => {
                let mut c = Aes192Ctr32BE::new(key.into(), iv.into());
                c.apply_keystream(&mut out);
            }
            32 => {
                let mut c = Aes256Ctr32BE::new(key.into(), iv.into());
                c.apply_keystream(&mut out);
            }
            n => return Err(format!("aes_ctr: key must be 16/24/32 bytes, got {n}")),
        }
        Ok(out)
    }

    // ---- SIV ----

    /// Deterministic SIV encrypt per RFC 5297. The `headers` iterator
    /// is the AD vector  for our scalar surface there's at most one
    /// AAD blob, so the iterator is empty (no AAD) or a single-item
    /// slice. Output layout = 16-byte S2V tag || ciphertext.
    fn siv_encrypt(key: &[u8], pt: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let headers: &[&[u8]] = if aad.is_empty() { &[] } else { core::slice::from_ref(&aad) };
        match key.len() {
            32 => {
                let mut cipher = Aes128Siv::new(GenericArray::from_slice(key));
                cipher
                    .encrypt(headers, pt)
                    .map_err(|e| format!("aes_siv_encrypt: {e}"))
            }
            64 => {
                let mut cipher = Aes256Siv::new(GenericArray::from_slice(key));
                cipher
                    .encrypt(headers, pt)
                    .map_err(|e| format!("aes_siv_encrypt: {e}"))
            }
            n => Err(format!("aes_siv_encrypt: key must be 32 or 64 bytes, got {n}")),
        }
    }

    fn siv_decrypt(key: &[u8], ct: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        let headers: &[&[u8]] = if aad.is_empty() { &[] } else { core::slice::from_ref(&aad) };
        match key.len() {
            32 => {
                let mut cipher = Aes128Siv::new(GenericArray::from_slice(key));
                cipher.decrypt(headers, ct).ok()
            }
            64 => {
                let mut cipher = Aes256Siv::new(GenericArray::from_slice(key));
                cipher.decrypt(headers, ct).ok()
            }
            _ => None,
        }
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
                name: "aes-modes".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_CBC_ENC, "aes_cbc_encrypt", 3, det),
                    s(FID_CBC_DEC, "aes_cbc_decrypt", 3, det),
                    s(FID_CTR_ENC, "aes_ctr_encrypt", 3, det),
                    s(FID_CTR_DEC, "aes_ctr_decrypt", 3, det),
                    // SIV takes optional AAD  num_args=-1 makes it
                    // variadic so the SQL caller can write either form.
                    s(FID_SIV_ENC, "aes_siv_encrypt", -1, det),
                    s(FID_SIV_DEC, "aes_siv_decrypt", -1, det),
                    s(FID_VERSION, "aes_modes_version", 0, det),
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
                FID_CBC_ENC => {
                    let key = arg_bytes(&args, 0, "aes_cbc_encrypt")?;
                    let iv = arg_bytes(&args, 1, "aes_cbc_encrypt")?;
                    let pt = arg_bytes(&args, 2, "aes_cbc_encrypt")?;
                    let iv16 = check_iv(&iv, "aes_cbc_encrypt")?;
                    cbc_encrypt(&key, &iv16, &pt).map(SqlValue::Blob)
                }
                FID_CBC_DEC => {
                    let key = arg_bytes(&args, 0, "aes_cbc_decrypt")?;
                    let iv = arg_bytes(&args, 1, "aes_cbc_decrypt")?;
                    let ct = arg_bytes(&args, 2, "aes_cbc_decrypt")?;
                    // Bad IV length / bad key length / bad pad all
                    // collapse to NULL  same "did this decrypt
                    // cleanly?" contract used by the aead crate.
                    let iv16 = match check_iv(&iv, "aes_cbc_decrypt") {
                        Ok(v) => v,
                        Err(_) => return Ok(SqlValue::Null),
                    };
                    Ok(match cbc_decrypt(&key, &iv16, &ct) {
                        Some(pt) => SqlValue::Blob(pt),
                        None => SqlValue::Null,
                    })
                }
                FID_CTR_ENC => {
                    let key = arg_bytes(&args, 0, "aes_ctr_encrypt")?;
                    let nonce = arg_bytes(&args, 1, "aes_ctr_encrypt")?;
                    let pt = arg_bytes(&args, 2, "aes_ctr_encrypt")?;
                    let iv = ctr_iv_from_nonce(&nonce, "aes_ctr_encrypt")?;
                    ctr_crypt(&key, &iv, &pt).map(SqlValue::Blob)
                }
                FID_CTR_DEC => {
                    let key = arg_bytes(&args, 0, "aes_ctr_decrypt")?;
                    let nonce = arg_bytes(&args, 1, "aes_ctr_decrypt")?;
                    let ct = arg_bytes(&args, 2, "aes_ctr_decrypt")?;
                    let iv = ctr_iv_from_nonce(&nonce, "aes_ctr_decrypt")?;
                    ctr_crypt(&key, &iv, &ct).map(SqlValue::Blob)
                }
                FID_SIV_ENC => {
                    let key = arg_bytes(&args, 0, "aes_siv_encrypt")?;
                    let pt = arg_bytes(&args, 1, "aes_siv_encrypt")?;
                    let aad = opt_aad(&args, 2);
                    siv_encrypt(&key, &pt, &aad).map(SqlValue::Blob)
                }
                FID_SIV_DEC => {
                    let key = arg_bytes(&args, 0, "aes_siv_decrypt")?;
                    let ct = arg_bytes(&args, 1, "aes_siv_decrypt")?;
                    let aad = opt_aad(&args, 2);
                    Ok(match siv_decrypt(&key, &ct, &aad) {
                        Some(pt) => SqlValue::Blob(pt),
                        None => SqlValue::Null,
                    })
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("aes-modes: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
