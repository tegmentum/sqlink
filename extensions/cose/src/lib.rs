//! CBOR Object Signing and Encryption (RFC 8152) for SQL.
//!
//! Sign/MAC functions emit tagged COSE structures (CoseSign1 = tag
//! 18, CoseMac0 = tag 17, CoseEncrypt0 = tag 16) so callers can
//! pipe the blobs to any RFC-8152-conformant decoder.
//!
//! Algorithm naming follows JWT/JOSE conventions (ES256, EdDSA,
//! HS256, HS512, A128GCM/A192GCM/A256GCM); the on-wire COSE alg
//! id stored in the protected header is the IANA-registered
//! numeric value.
//!
//! `cose_verify1` / `cose_decrypt0` / `cose_mac0_verify` return
//! NULL on any failure mode (bad sig, tampered ciphertext, wrong
//! key, malformed CBOR, header alg mismatch, missing nonce, ...)
//!   the SQL contract is "did this validate, yes or no" — there's
//! no signal channel for distinguishing "bad sig" from "malformed
//! input" without widening the surface to error rows.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use coset::{
    iana, CborSerializable, CoseEncrypt0, CoseEncrypt0Builder, CoseMac0, CoseMac0Builder,
    CoseSign1, CoseSign1Builder, HeaderBuilder, TaggedCborSerializable,
};

// ─────────────── algorithm parsing ───────────────

/// JOSE-style algorithm name → IANA COSE alg numeric id + which
/// category (sign / mac / aead). Bundling category here keeps the
/// dispatchers small and prevents "asked for HS256 in cose_sign1"
/// from looking like a successful sign that happens to use HMAC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alg {
    ES256,
    EdDSA,
    HS256,
    HS512,
    A128GCM,
    A192GCM,
    A256GCM,
}

impl Alg {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "ES256" => Some(Alg::ES256),
            // Per RFC 8037 "EdDSA" is the JOSE name and "Ed25519"
            // is the curve; accept both — different libraries
            // emit either.
            "EdDSA" | "Ed25519" => Some(Alg::EdDSA),
            "HS256" => Some(Alg::HS256),
            "HS512" => Some(Alg::HS512),
            "A128GCM" => Some(Alg::A128GCM),
            "A192GCM" => Some(Alg::A192GCM),
            "A256GCM" => Some(Alg::A256GCM),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Alg::ES256 => "ES256",
            Alg::EdDSA => "EdDSA",
            Alg::HS256 => "HS256",
            Alg::HS512 => "HS512",
            Alg::A128GCM => "A128GCM",
            Alg::A192GCM => "A192GCM",
            Alg::A256GCM => "A256GCM",
        }
    }

    pub fn iana(self) -> iana::Algorithm {
        match self {
            Alg::ES256 => iana::Algorithm::ES256,
            Alg::EdDSA => iana::Algorithm::EdDSA,
            Alg::HS256 => iana::Algorithm::HMAC_256_256,
            Alg::HS512 => iana::Algorithm::HMAC_512_512,
            Alg::A128GCM => iana::Algorithm::A128GCM,
            Alg::A192GCM => iana::Algorithm::A192GCM,
            Alg::A256GCM => iana::Algorithm::A256GCM,
        }
    }

    pub fn from_iana(v: i64) -> Option<Self> {
        // Hand-roll the inverse rather than depend on iana::Algorithm
        // having a `from_i64`. The numeric values are stable RFC
        // 8152 registrations.
        match v {
            -7 => Some(Alg::ES256),
            -8 => Some(Alg::EdDSA),
            5 => Some(Alg::HS256),
            7 => Some(Alg::HS512),
            1 => Some(Alg::A128GCM),
            2 => Some(Alg::A192GCM),
            3 => Some(Alg::A256GCM),
            _ => None,
        }
    }
}

// ─────────────── signing primitives ───────────────

fn hmac_sign(alg: Alg, key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    use hmac::{Hmac, Mac};
    match alg {
        Alg::HS256 => {
            let mut m = <Hmac<sha2::Sha256>>::new_from_slice(key)
                .map_err(|e| format!("hmac key: {e}"))?;
            m.update(data);
            Ok(m.finalize().into_bytes().to_vec())
        }
        Alg::HS512 => {
            let mut m = <Hmac<sha2::Sha512>>::new_from_slice(key)
                .map_err(|e| format!("hmac key: {e}"))?;
            m.update(data);
            Ok(m.finalize().into_bytes().to_vec())
        }
        _ => Err("hmac_sign: alg is not an HMAC".into()),
    }
}

fn hmac_verify(alg: Alg, key: &[u8], data: &[u8], tag: &[u8]) -> bool {
    use hmac::{Hmac, Mac};
    match alg {
        Alg::HS256 => {
            let mut m = match <Hmac<sha2::Sha256>>::new_from_slice(key) {
                Ok(m) => m,
                Err(_) => return false,
            };
            m.update(data);
            m.verify_slice(tag).is_ok()
        }
        Alg::HS512 => {
            let mut m = match <Hmac<sha2::Sha512>>::new_from_slice(key) {
                Ok(m) => m,
                Err(_) => return false,
            };
            m.update(data);
            m.verify_slice(tag).is_ok()
        }
        _ => false,
    }
}

fn ed25519_sign(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    use ed25519_dalek::Signer;
    let seed: &[u8] = match key.len() {
        32 => key,
        64 => &key[..32],
        n => return Err(format!("ed25519 key must be 32 or 64 bytes, got {n}")),
    };
    let arr: [u8; 32] = seed
        .try_into()
        .map_err(|_| "ed25519 seed length mismatch".to_string())?;
    let sk = ed25519_dalek::SigningKey::from_bytes(&arr);
    Ok(sk.sign(data).to_bytes().to_vec())
}

fn ed25519_verify(pubkey: &[u8], data: &[u8], sig: &[u8]) -> bool {
    use ed25519_dalek::Verifier;
    let arr: [u8; 32] = match pubkey.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let vk = match ed25519_dalek::VerifyingKey::from_bytes(&arr) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let sig_bytes: [u8; 64] = match sig.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let s = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    vk.verify(data, &s).is_ok()
}

/// ES256 = ECDSA over P-256 with SHA-256. COSE uses the JOSE-style
/// raw r||s 64-byte signature, NOT the ASN.1 DER form most ECDSA
/// libraries default to (RFC 8152 § 8.1).
fn es256_sign(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    use p256::ecdsa::{signature::Signer, Signature, SigningKey};
    if key.len() != 32 {
        return Err(format!(
            "ES256 private key must be 32 bytes, got {}",
            key.len()
        ));
    }
    let sk = SigningKey::from_slice(key)
        .map_err(|e| format!("ES256 signing key: {e}"))?;
    let sig: Signature = sk.sign(data);
    // .to_bytes() returns the fixed-size r||s 64-byte form — exactly
    // what COSE expects.
    Ok(sig.to_bytes().to_vec())
}

fn es256_verify(pubkey: &[u8], data: &[u8], sig: &[u8]) -> bool {
    use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
    // Accept 33-byte SEC1 compressed, 65-byte SEC1 uncompressed,
    // or 64-byte raw x||y (we prepend 0x04 to fake uncompressed).
    let vk = match pubkey.len() {
        33 | 65 => match VerifyingKey::from_sec1_bytes(pubkey) {
            Ok(v) => v,
            Err(_) => return false,
        },
        64 => {
            let mut buf = vec![0x04u8];
            buf.extend_from_slice(pubkey);
            match VerifyingKey::from_sec1_bytes(&buf) {
                Ok(v) => v,
                Err(_) => return false,
            }
        }
        _ => return false,
    };
    if sig.len() != 64 {
        return false;
    }
    let s = match Signature::from_slice(sig) {
        Ok(s) => s,
        Err(_) => return false,
    };
    vk.verify(data, &s).is_ok()
}

// ─────────────── AES-GCM ───────────────

fn aes_key_len(alg: Alg) -> usize {
    match alg {
        Alg::A128GCM => 16,
        Alg::A192GCM => 24,
        Alg::A256GCM => 32,
        _ => 0,
    }
}

fn aes_gcm_encrypt(
    alg: Alg,
    key: &[u8],
    nonce: &[u8; 12],
    pt: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce};
    let expected = aes_key_len(alg);
    if key.len() != expected {
        return Err(format!(
            "{}: key must be {} bytes, got {}",
            alg.as_str(),
            expected,
            key.len()
        ));
    }
    match alg {
        Alg::A128GCM => {
            let cipher = Aes128Gcm::new_from_slice(key)
                .map_err(|e| format!("A128GCM key: {e}"))?;
            cipher
                .encrypt(Nonce::from_slice(nonce), Payload { msg: pt, aad })
                .map_err(|e| format!("A128GCM encrypt: {e}"))
        }
        Alg::A256GCM => {
            let cipher = Aes256Gcm::new_from_slice(key)
                .map_err(|e| format!("A256GCM key: {e}"))?;
            cipher
                .encrypt(Nonce::from_slice(nonce), Payload { msg: pt, aad })
                .map_err(|e| format!("A256GCM encrypt: {e}"))
        }
        // A192GCM: aes-gcm 0.10 doesn't export an Aes192Gcm type
        // alias. Hand-wire via aes::Aes192 + AesGcm<_, U12>.
        Alg::A192GCM => {
            use aes_gcm::aead::generic_array::typenum::U12;
            use aes_gcm::aes::Aes192;
            use aes_gcm::AesGcm;
            type Aes192Gcm = AesGcm<Aes192, U12>;
            let cipher = Aes192Gcm::new_from_slice(key)
                .map_err(|e| format!("A192GCM key: {e}"))?;
            cipher
                .encrypt(Nonce::from_slice(nonce), Payload { msg: pt, aad })
                .map_err(|e| format!("A192GCM encrypt: {e}"))
        }
        _ => Err("aes_gcm_encrypt: not an AES-GCM alg".into()),
    }
}

fn aes_gcm_decrypt(
    alg: Alg,
    key: &[u8],
    nonce: &[u8; 12],
    ct: &[u8],
    aad: &[u8],
) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce};
    if key.len() != aes_key_len(alg) {
        return None;
    }
    match alg {
        Alg::A128GCM => {
            let cipher = Aes128Gcm::new_from_slice(key).ok()?;
            cipher
                .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
                .ok()
        }
        Alg::A256GCM => {
            let cipher = Aes256Gcm::new_from_slice(key).ok()?;
            cipher
                .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
                .ok()
        }
        Alg::A192GCM => {
            use aes_gcm::aead::generic_array::typenum::U12;
            use aes_gcm::aes::Aes192;
            use aes_gcm::AesGcm;
            type Aes192Gcm = AesGcm<Aes192, U12>;
            let cipher = Aes192Gcm::new_from_slice(key).ok()?;
            cipher
                .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
                .ok()
        }
        _ => None,
    }
}

// ─────────────── randomness ───────────────

fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; n];
    getrandom::getrandom(&mut out).map_err(|e| format!("getrandom: {e}"))?;
    Ok(out)
}

// ─────────────── COSE_Sign1 ───────────────

pub fn cose_sign1(payload: &[u8], key: &[u8], alg: Alg) -> Result<Vec<u8>, String> {
    let protected = HeaderBuilder::new().algorithm(alg.iana()).build();
    let builder = CoseSign1Builder::new()
        .protected(protected)
        .payload(payload.to_vec());
    let sign1 = match alg {
        Alg::ES256 => builder.try_create_signature(b"", |tbs| es256_sign(key, tbs))?,
        Alg::EdDSA => builder.try_create_signature(b"", |tbs| ed25519_sign(key, tbs))?,
        _ => {
            return Err(format!(
                "cose_sign1: alg {} is not a signing alg",
                alg.as_str()
            ))
        }
    }
    .build();
    // Tagged form so callers (and round-trip-into-coset
    // CoseSign1::from_tagged_slice) recognize the structure
    // without an out-of-band type hint.
    sign1
        .to_tagged_vec()
        .map_err(|e| format!("cose_sign1: serialize: {e}"))
}

/// Returns Some(payload) on a valid signature. None on every other
/// outcome — malformed CBOR, header alg mismatch, sig check fail,
/// wrong key length, etc.
pub fn cose_verify1(blob: &[u8], key: &[u8], alg: Alg) -> Option<Vec<u8>> {
    // Accept both tagged and untagged input — from_slice is
    // permissive but from_tagged_slice insists on the tag. Try
    // tagged first (the form cose_sign1 emits), fall back to
    // untagged for interop.
    let s = CoseSign1::from_tagged_slice(blob)
        .or_else(|_| CoseSign1::from_slice(blob))
        .ok()?;
    // Defense against alg downgrade: the on-wire alg must match
    // the caller's requested alg (RFC 8725 spirit applied to
    // COSE).
    let hdr_alg = match &s.protected.header.alg {
        Some(coset::Algorithm::Assigned(a)) => *a as i64,
        _ => return None,
    };
    if Alg::from_iana(hdr_alg) != Some(alg) {
        return None;
    }
    let ok = match alg {
        Alg::ES256 => s
            .verify_signature(b"", |sig, data| {
                if es256_verify(key, data, sig) {
                    Ok(())
                } else {
                    Err(())
                }
            })
            .is_ok(),
        Alg::EdDSA => s
            .verify_signature(b"", |sig, data| {
                if ed25519_verify(key, data, sig) {
                    Ok(())
                } else {
                    Err(())
                }
            })
            .is_ok(),
        _ => false,
    };
    if !ok {
        return None;
    }
    s.payload
}

// ─────────────── COSE_Mac0 ───────────────

pub fn cose_mac0(payload: &[u8], key: &[u8], alg: Alg) -> Result<Vec<u8>, String> {
    match alg {
        Alg::HS256 | Alg::HS512 => {}
        _ => return Err(format!("cose_mac0: alg {} is not a MAC alg", alg.as_str())),
    }
    let protected = HeaderBuilder::new().algorithm(alg.iana()).build();
    let mac0 = CoseMac0Builder::new()
        .protected(protected)
        .payload(payload.to_vec())
        .try_create_tag(b"", |tbm| hmac_sign(alg, key, tbm))?
        .build();
    mac0.to_tagged_vec()
        .map_err(|e| format!("cose_mac0: serialize: {e}"))
}

/// Returns Some(payload) when the tag verifies; None on any
/// failure. Same contract as cose_verify1.
pub fn cose_mac0_verify(blob: &[u8], key: &[u8], alg: Alg) -> Option<Vec<u8>> {
    let m = CoseMac0::from_tagged_slice(blob)
        .or_else(|_| CoseMac0::from_slice(blob))
        .ok()?;
    let hdr_alg = match &m.protected.header.alg {
        Some(coset::Algorithm::Assigned(a)) => *a as i64,
        _ => return None,
    };
    if Alg::from_iana(hdr_alg) != Some(alg) {
        return None;
    }
    let ok = m
        .verify_tag(b"", |tag, data| {
            if hmac_verify(alg, key, data, tag) {
                Ok(())
            } else {
                Err(())
            }
        })
        .is_ok();
    if !ok {
        return None;
    }
    m.payload
}

// ─────────────── COSE_Encrypt0 ───────────────

pub fn cose_encrypt0(plaintext: &[u8], key: &[u8], alg: Alg) -> Result<Vec<u8>, String> {
    match alg {
        Alg::A128GCM | Alg::A192GCM | Alg::A256GCM => {}
        _ => {
            return Err(format!(
                "cose_encrypt0: alg {} is not an AEAD alg",
                alg.as_str()
            ))
        }
    }
    let expected_key = aes_key_len(alg);
    if key.len() != expected_key {
        return Err(format!(
            "cose_encrypt0: {} key must be {} bytes, got {}",
            alg.as_str(),
            expected_key,
            key.len()
        ));
    }
    // 96-bit nonce per RFC 8152 § 10.1 (and NIST SP 800-38D).
    let nonce_vec = random_bytes(12)?;
    let nonce: [u8; 12] = nonce_vec
        .as_slice()
        .try_into()
        .map_err(|_| "cose_encrypt0: nonce length".to_string())?;
    let protected = HeaderBuilder::new().algorithm(alg.iana()).build();
    // The IV lives in the unprotected header so the receiver can
    // read it before authenticating (decrypt0 needs it to call
    // the AEAD); putting it in the protected header would mean
    // re-deriving the AAD on the way out — extra round-trips for
    // no security gain since the IV is bound into the GCM tag
    // anyway.
    let unprotected = HeaderBuilder::new().iv(nonce_vec.clone()).build();
    let enc0 = CoseEncrypt0Builder::new()
        .protected(protected)
        .unprotected(unprotected)
        .try_create_ciphertext(plaintext, b"", |pt, aad| {
            aes_gcm_encrypt(alg, key, &nonce, pt, aad)
        })?
        .build();
    enc0.to_tagged_vec()
        .map_err(|e| format!("cose_encrypt0: serialize: {e}"))
}

pub fn cose_decrypt0(blob: &[u8], key: &[u8], alg: Alg) -> Option<Vec<u8>> {
    let e = CoseEncrypt0::from_tagged_slice(blob)
        .or_else(|_| CoseEncrypt0::from_slice(blob))
        .ok()?;
    let hdr_alg = match &e.protected.header.alg {
        Some(coset::Algorithm::Assigned(a)) => *a as i64,
        _ => return None,
    };
    if Alg::from_iana(hdr_alg) != Some(alg) {
        return None;
    }
    if e.unprotected.iv.len() != 12 {
        return None;
    }
    let nonce: [u8; 12] = match e.unprotected.iv.as_slice().try_into() {
        Ok(n) => n,
        Err(_) => return None,
    };
    e.decrypt(b"", |ct, aad| {
        aes_gcm_decrypt(alg, key, &nonce, ct, aad).ok_or(())
    })
    .ok()
}

// ─────────────── cose_inspect ───────────────

/// Decode a tagged COSE blob into a JSON summary. Best-effort —
/// returns a structured error JSON rather than failing the SQL
/// row, so SELECT cose_inspect(blob) FROM ... never throws.
pub fn cose_inspect(blob: &[u8]) -> String {
    // Sniff the CBOR tag without committing to a specific COSE
    // type. ciborium will surface Value::Tag(n, inner) for
    // self-describing input — we just look at the outermost.
    use ciborium::Value;
    let parsed: Result<Value, _> = ciborium::de::from_reader(blob);
    let outer = match parsed {
        Ok(v) => v,
        Err(e) => return error_json(&format!("cbor decode: {e}")),
    };
    let tag = match &outer {
        Value::Tag(t, _) => Some(*t),
        _ => None,
    };
    let (kind, hdrs_payload) = match tag {
        Some(18) => match decode_sign1(blob) {
            Some(x) => ("CoseSign1", x),
            None => return error_json("CoseSign1 tag present but parse failed"),
        },
        Some(17) => match decode_mac0(blob) {
            Some(x) => ("CoseMac0", x),
            None => return error_json("CoseMac0 tag present but parse failed"),
        },
        Some(16) => match decode_encrypt0(blob) {
            Some(x) => ("CoseEncrypt0", x),
            None => return error_json("CoseEncrypt0 tag present but parse failed"),
        },
        _ => {
            // No COSE tag — try each type in turn against the
            // untagged form.
            if let Some(x) = decode_sign1(blob) {
                ("CoseSign1", x)
            } else if let Some(x) = decode_mac0(blob) {
                ("CoseMac0", x)
            } else if let Some(x) = decode_encrypt0(blob) {
                ("CoseEncrypt0", x)
            } else {
                return error_json("unrecognized COSE structure");
            }
        }
    };
    let (hdrs, payload_info) = hdrs_payload;
    let mut out = serde_json::Map::new();
    out.insert("kind".into(), serde_json::Value::String(kind.into()));
    if let Some(t) = tag {
        out.insert("tag".into(), serde_json::Value::Number(t.into()));
    }
    out.insert("protected".into(), hdrs.0);
    out.insert("unprotected".into(), hdrs.1);
    out.insert("payload".into(), payload_info);
    serde_json::Value::Object(out).to_string()
}

fn error_json(msg: &str) -> String {
    let mut m = serde_json::Map::new();
    m.insert("error".into(), serde_json::Value::String(msg.into()));
    serde_json::Value::Object(m).to_string()
}

type Hdrs = (serde_json::Value, serde_json::Value);

fn decode_sign1(blob: &[u8]) -> Option<(Hdrs, serde_json::Value)> {
    let s = CoseSign1::from_tagged_slice(blob)
        .or_else(|_| CoseSign1::from_slice(blob))
        .ok()?;
    let prot = header_to_json(&s.protected.header);
    let unprot = header_to_json(&s.unprotected);
    let payload = match s.payload {
        Some(p) => payload_summary(&p),
        None => serde_json::Value::Null,
    };
    Some(((prot, unprot), payload))
}

fn decode_mac0(blob: &[u8]) -> Option<(Hdrs, serde_json::Value)> {
    let m = CoseMac0::from_tagged_slice(blob)
        .or_else(|_| CoseMac0::from_slice(blob))
        .ok()?;
    let prot = header_to_json(&m.protected.header);
    let unprot = header_to_json(&m.unprotected);
    let payload = match m.payload {
        Some(p) => payload_summary(&p),
        None => serde_json::Value::Null,
    };
    Some(((prot, unprot), payload))
}

fn decode_encrypt0(blob: &[u8]) -> Option<(Hdrs, serde_json::Value)> {
    let e = CoseEncrypt0::from_tagged_slice(blob)
        .or_else(|_| CoseEncrypt0::from_slice(blob))
        .ok()?;
    let prot = header_to_json(&e.protected.header);
    let unprot = header_to_json(&e.unprotected);
    let payload = match e.ciphertext {
        Some(c) => {
            let mut m = serde_json::Map::new();
            m.insert(
                "ciphertext_len".into(),
                serde_json::Value::Number(c.len().into()),
            );
            // Cap hex dump at 1 KiB so SELECT * doesn't choke on
            // megabyte ciphertexts. Length above is exact.
            let take = c.len().min(1024);
            m.insert(
                "ciphertext_hex".into(),
                serde_json::Value::String(hex::encode(&c[..take])),
            );
            if c.len() > take {
                m.insert("truncated".into(), serde_json::Value::Bool(true));
            }
            serde_json::Value::Object(m)
        }
        None => serde_json::Value::Null,
    };
    Some(((prot, unprot), payload))
}

fn payload_summary(p: &[u8]) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("len".into(), serde_json::Value::Number(p.len().into()));
    // If it parses as UTF-8 it's useful to show it. Most COSE
    // payloads in practice are CBOR-encoded claims (CWTs) — those
    // won't be valid UTF-8 and fall through to hex.
    if let Ok(s) = core::str::from_utf8(p) {
        let take = s.len().min(1024);
        m.insert(
            "text".into(),
            serde_json::Value::String(s[..take].to_string()),
        );
        if s.len() > take {
            m.insert("truncated".into(), serde_json::Value::Bool(true));
        }
    } else {
        let take = p.len().min(512);
        m.insert(
            "hex".into(),
            serde_json::Value::String(hex::encode(&p[..take])),
        );
        if p.len() > take {
            m.insert("truncated".into(), serde_json::Value::Bool(true));
        }
    }
    serde_json::Value::Object(m)
}

fn header_to_json(h: &coset::Header) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    if let Some(coset::Algorithm::Assigned(a)) = &h.alg {
        let n = *a as i64;
        m.insert("alg".into(), serde_json::Value::Number(n.into()));
        if let Some(named) = Alg::from_iana(n) {
            m.insert(
                "alg_name".into(),
                serde_json::Value::String(named.as_str().into()),
            );
        }
    }
    if !h.key_id.is_empty() {
        m.insert(
            "kid_hex".into(),
            serde_json::Value::String(hex::encode(&h.key_id)),
        );
    }
    if !h.iv.is_empty() {
        m.insert(
            "iv_hex".into(),
            serde_json::Value::String(hex::encode(&h.iv)),
        );
    }
    if !h.partial_iv.is_empty() {
        m.insert(
            "partial_iv_hex".into(),
            serde_json::Value::String(hex::encode(&h.partial_iv)),
        );
    }
    if let Some(ct) = &h.content_type {
        let v = match ct {
            coset::ContentType::Text(s) => serde_json::Value::String(s.clone()),
            coset::ContentType::Assigned(a) => {
                serde_json::Value::Number((*a as i64).into())
            }
            // Newer coset versions may add variants; treat
            // unknown as a stringified Debug to stay forward-
            // compatible.
            #[allow(unreachable_patterns)]
            other => serde_json::Value::String(format!("{other:?}")),
        };
        m.insert("content_type".into(), v);
    }
    serde_json::Value::Object(m)
}

// ─────────────── wasm component export ───────────────

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

    const FID_SIGN1: u64 = 1;
    const FID_VERIFY1: u64 = 2;
    const FID_ENCRYPT0: u64 = 3;
    const FID_DECRYPT0: u64 = 4;
    const FID_MAC0: u64 = 5;
    const FID_MAC0_VRF: u64 = 6;
    const FID_INSPECT: u64 = 7;
    const FID_VERSION: u64 = 8;

    struct Ext;

    fn arg_bytes(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB/TEXT arg at {i}")),
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            // encrypt0 mixes a fresh random nonce into the output;
            // it must NOT be flagged deterministic or the planner
            // will memoize it.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "cose".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_SIGN1, "cose_sign1", 3, det),
                    s(FID_VERIFY1, "cose_verify1", 3, det),
                    s(FID_ENCRYPT0, "cose_encrypt0", 3, nd),
                    s(FID_DECRYPT0, "cose_decrypt0", 3, det),
                    s(FID_MAC0, "cose_mac0", 3, det),
                    s(FID_MAC0_VRF, "cose_mac0_verify", 3, det),
                    s(FID_INSPECT, "cose_inspect", 1, det),
                    s(FID_VERSION, "cose_version", 0, det),
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

    fn parse_alg(s: &str, fname: &str) -> Result<super::Alg, String> {
        super::Alg::from_str(s)
            .ok_or_else(|| format!("{fname}: unsupported alg {s:?}"))
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_SIGN1 => {
                    let payload = arg_bytes(&args, 0, "cose_sign1")?;
                    let key = arg_bytes(&args, 1, "cose_sign1")?;
                    let alg = parse_alg(&arg_text(&args, 2, "cose_sign1")?, "cose_sign1")?;
                    super::cose_sign1(&payload, &key, alg).map(SqlValue::Blob)
                }
                FID_VERIFY1 => {
                    let blob = arg_bytes(&args, 0, "cose_verify1")?;
                    let key = arg_bytes(&args, 1, "cose_verify1")?;
                    // Unknown alg ⇒ NULL (same contract as "sig
                    // didn't verify") so callers don't need to
                    // wrap in error handling.
                    let alg_s = arg_text(&args, 2, "cose_verify1")?;
                    let alg = match super::Alg::from_str(&alg_s) {
                        Some(a) => a,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match super::cose_verify1(&blob, &key, alg) {
                        Some(p) => SqlValue::Blob(p),
                        None => SqlValue::Null,
                    })
                }
                FID_ENCRYPT0 => {
                    let pt = arg_bytes(&args, 0, "cose_encrypt0")?;
                    let key = arg_bytes(&args, 1, "cose_encrypt0")?;
                    let alg =
                        parse_alg(&arg_text(&args, 2, "cose_encrypt0")?, "cose_encrypt0")?;
                    super::cose_encrypt0(&pt, &key, alg).map(SqlValue::Blob)
                }
                FID_DECRYPT0 => {
                    let blob = arg_bytes(&args, 0, "cose_decrypt0")?;
                    let key = arg_bytes(&args, 1, "cose_decrypt0")?;
                    let alg_s = arg_text(&args, 2, "cose_decrypt0")?;
                    let alg = match super::Alg::from_str(&alg_s) {
                        Some(a) => a,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match super::cose_decrypt0(&blob, &key, alg) {
                        Some(p) => SqlValue::Blob(p),
                        None => SqlValue::Null,
                    })
                }
                FID_MAC0 => {
                    let payload = arg_bytes(&args, 0, "cose_mac0")?;
                    let key = arg_bytes(&args, 1, "cose_mac0")?;
                    let alg = parse_alg(&arg_text(&args, 2, "cose_mac0")?, "cose_mac0")?;
                    super::cose_mac0(&payload, &key, alg).map(SqlValue::Blob)
                }
                FID_MAC0_VRF => {
                    let blob = arg_bytes(&args, 0, "cose_mac0_verify")?;
                    let key = arg_bytes(&args, 1, "cose_mac0_verify")?;
                    let alg_s = arg_text(&args, 2, "cose_mac0_verify")?;
                    let alg = match super::Alg::from_str(&alg_s) {
                        Some(a) => a,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match super::cose_mac0_verify(&blob, &key, alg) {
                        Some(p) => SqlValue::Blob(p),
                        None => SqlValue::Null,
                    })
                }
                FID_INSPECT => {
                    let blob = arg_bytes(&args, 0, "cose_inspect")?;
                    Ok(SqlValue::Text(super::cose_inspect(&blob)))
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("cose: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
