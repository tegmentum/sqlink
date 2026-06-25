//! Hand-rolled WebAuthn (Level 2/3) registration + authentication
//! verification for SQL. Does NOT depend on webauthn-rs  that crate
//! pulls in `openssl-sys`, which has no usable cross-compile to
//! wasm32-wasip2. We implement just enough of the spec to verify
//! ES256 / EdDSA / RS256 attestations + assertions against the
//! authenticator-data + clientDataJSON binding.
//!
//! Coverage versus the full WebAuthn spec:
//!
//!   IMPLEMENTED
//!     * registration options builder (PublicKeyCredentialCreationOptions)
//!     * authentication options builder (PublicKeyCredentialRequestOptions)
//!     * registration verify:
//!         - clientDataJSON type/challenge/origin checks
//!         - rpIdHash check
//!         - flags.up assertion
//!         - attestedCredentialData extraction (aaguid, credId, COSE_Key)
//!         - attestation signature verify for "none" + "packed" self-attest
//!           (chain trust is deferred  see note below)
//!         - returns the credential public key as a portable JSON blob the
//!           authentication step can consume
//!     * authentication verify:
//!         - clientDataJSON checks
//!         - rpIdHash + flags.up
//!         - signature verify over authData || sha256(clientDataJSON)
//!         - sign-count anti-cloning check (rejects when counter <=
//!           expected AND the authenticator reports a non-zero counter;
//!           matches the spec's "MAY reject" guidance hardened into a
//!           MUST)
//!
//!   DEFERRED (documented in Cargo.toml description; do not regress
//!   silently  pair this extension with an out-of-band attestation-
//!   trust policy if you need WebAuthn-Strict semantics):
//!     * attestation certificate chain validation + AAGUID lookup
//!     * Apple/Android/TPM/FIDO-U2F attestation formats
//!     * extension processing (largeBlob, credProps, ...)
//!     * conditional UI (`mediation: "conditional"`)
//!
//! All four verify scalars return NULL on ANY failure (bad sig,
//! tampered authData, malformed JSON, wrong origin, sign-count
//! regression, unsupported alg, ...) so SQL callers can wrap them in
//! CASE / WHERE without juggling error rows.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::Digest as _;

// ─────────────── base64url helpers ───────────────

pub fn b64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64url_decode(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(s.as_bytes())
        .map_err(|e| format!("base64url decode: {e}"))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

// ─────────────── COSE key parsing ───────────────
//
// CTAP2 / WebAuthn embed credential public keys as CBOR-encoded
// COSE_Keys with integer map keys (RFC 8152 § 7). We only need the
// public-key parameters for ES256 (-7), EdDSA (-8) and RS256 (-257).

#[derive(Clone, Debug)]
pub enum CoseKey {
    Es256 { x: [u8; 32], y: [u8; 32] },
    Ed25519 { x: [u8; 32] },
    Rs256 { n: Vec<u8>, e: Vec<u8> },
}

impl CoseKey {
    pub fn alg_str(&self) -> &'static str {
        match self {
            CoseKey::Es256 { .. } => "ES256",
            CoseKey::Ed25519 { .. } => "EdDSA",
            CoseKey::Rs256 { .. } => "RS256",
        }
    }

    /// Encode as a self-contained JSON object so the caller can stuff
    /// it into a column + feed it back to webauthn_verify_authentication
    /// without juggling a CBOR blob.
    pub fn to_json(&self) -> String {
        let mut m = serde_json::Map::new();
        m.insert("alg".into(), serde_json::Value::String(self.alg_str().into()));
        match self {
            CoseKey::Es256 { x, y } => {
                m.insert("kty".into(), serde_json::Value::String("EC2".into()));
                m.insert("crv".into(), serde_json::Value::String("P-256".into()));
                m.insert("x".into(), serde_json::Value::String(b64url_encode(x)));
                m.insert("y".into(), serde_json::Value::String(b64url_encode(y)));
            }
            CoseKey::Ed25519 { x } => {
                m.insert("kty".into(), serde_json::Value::String("OKP".into()));
                m.insert("crv".into(), serde_json::Value::String("Ed25519".into()));
                m.insert("x".into(), serde_json::Value::String(b64url_encode(x)));
            }
            CoseKey::Rs256 { n, e } => {
                m.insert("kty".into(), serde_json::Value::String("RSA".into()));
                m.insert("n".into(), serde_json::Value::String(b64url_encode(n)));
                m.insert("e".into(), serde_json::Value::String(b64url_encode(e)));
            }
        }
        serde_json::Value::Object(m).to_string()
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
        let v: serde_json::Value = serde_json::from_str(s)
            .map_err(|e| format!("CoseKey::from_json: {e}"))?;
        let obj = v.as_object()
            .ok_or_else(|| "CoseKey::from_json: expected object".to_string())?;
        let alg = obj.get("alg").and_then(|x| x.as_str())
            .ok_or_else(|| "CoseKey::from_json: missing alg".to_string())?;
        match alg {
            "ES256" => {
                let x = decode_b64_fixed::<32>(obj, "x")?;
                let y = decode_b64_fixed::<32>(obj, "y")?;
                Ok(CoseKey::Es256 { x, y })
            }
            "EdDSA" | "Ed25519" => {
                let x = decode_b64_fixed::<32>(obj, "x")?;
                Ok(CoseKey::Ed25519 { x })
            }
            "RS256" => {
                let n = decode_b64_var(obj, "n")?;
                let e = decode_b64_var(obj, "e")?;
                Ok(CoseKey::Rs256 { n, e })
            }
            other => Err(format!("CoseKey::from_json: unsupported alg {other:?}")),
        }
    }
}

fn decode_b64_fixed<const N: usize>(
    obj: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<[u8; N], String> {
    let s = obj.get(field).and_then(|v| v.as_str())
        .ok_or_else(|| format!("CoseKey: missing field {field}"))?;
    let raw = b64url_decode(s)?;
    if raw.len() != N {
        return Err(format!(
            "CoseKey: field {field} must be {N} bytes, got {}",
            raw.len()
        ));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&raw);
    Ok(out)
}

fn decode_b64_var(
    obj: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<u8>, String> {
    let s = obj.get(field).and_then(|v| v.as_str())
        .ok_or_else(|| format!("CoseKey: missing field {field}"))?;
    b64url_decode(s)
}

/// Parse a COSE_Key (CBOR map with integer keys) at the start of
/// `bytes`. Returns the parsed key and the byte length of the
/// CBOR-encoded prefix that contained the key (needed because
/// attestedCredentialData inlines the COSE_Key followed by optional
/// extensions and we need to know where the key ends).
pub fn parse_cose_key(bytes: &[u8]) -> Result<(CoseKey, usize), String> {
    use ciborium::value::Value;
    // Walk a custom reader that counts bytes consumed so we can
    // surface the end-of-key offset to the caller. ciborium stops at
    // the end of one CBOR item, so `pos` after `from_reader` is the
    // exact key-end offset.
    struct CountingReader<'a> {
        buf: &'a [u8],
        pos: usize,
    }
    impl<'a> ciborium_io::Read for CountingReader<'a> {
        type Error = EofError;
        fn read_exact(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
            if self.pos + dst.len() > self.buf.len() {
                return Err(EofError);
            }
            dst.copy_from_slice(&self.buf[self.pos..self.pos + dst.len()]);
            self.pos += dst.len();
            Ok(())
        }
    }
    #[derive(Debug)]
    struct EofError;
    impl core::fmt::Display for EofError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "unexpected EOF")
        }
    }

    let mut rdr = CountingReader { buf: bytes, pos: 0 };
    let v: Value = ciborium::de::from_reader(&mut rdr)
        .map_err(|e| format!("parse_cose_key: CBOR decode: {e:?}"))?;
    let consumed = rdr.pos;
    let map = match v {
        Value::Map(m) => m,
        _ => return Err("parse_cose_key: expected CBOR map".into()),
    };
    let mut kty: Option<i64> = None;
    let mut alg: Option<i64> = None;
    let mut crv: Option<i64> = None;
    let mut x: Option<Vec<u8>> = None;
    let mut y: Option<Vec<u8>> = None;
    let mut rsa_n: Option<Vec<u8>> = None;
    let rsa_e: Option<Vec<u8>>;
    for (k, v) in map {
        let label = match k {
            Value::Integer(i) => int_to_i64(i),
            _ => continue,
        };
        match label {
            1 => kty = v.as_integer().map(int_to_i64),
            3 => alg = v.as_integer().map(int_to_i64),
            -1 => match &v {
                Value::Integer(i) => crv = Some(int_to_i64(*i)),
                Value::Bytes(b) => rsa_n = Some(b.clone()), // RSA modulus
                _ => {}
            },
            -2 => match &v {
                Value::Bytes(b) => {
                    if x.is_none() {
                        x = Some(b.clone()); // EC2/OKP x OR RSA e (we disambiguate later)
                    }
                }
                _ => {}
            },
            -3 => match &v {
                Value::Bytes(b) => y = Some(b.clone()),
                _ => {}
            },
            _ => {}
        }
    }
    let kty = kty.ok_or_else(|| "parse_cose_key: missing kty".to_string())?;
    let alg = alg.ok_or_else(|| "parse_cose_key: missing alg".to_string())?;
    // RFC 8152 §13.1.1 (EC2 = 2), §13.2 (OKP = 1), §13.3.1 (RSA = 3).
    match (kty, alg) {
        (2, -7) => {
            if crv != Some(1) {
                return Err(format!("parse_cose_key: ES256 needs crv=P-256, got {crv:?}"));
            }
            let xb = x.ok_or_else(|| "ES256: missing x".to_string())?;
            let yb = y.ok_or_else(|| "ES256: missing y".to_string())?;
            if xb.len() != 32 || yb.len() != 32 {
                return Err("ES256: x/y must be 32 bytes".into());
            }
            let mut xa = [0u8; 32];
            xa.copy_from_slice(&xb);
            let mut ya = [0u8; 32];
            ya.copy_from_slice(&yb);
            Ok((CoseKey::Es256 { x: xa, y: ya }, consumed))
        }
        (1, -8) => {
            if crv != Some(6) {
                return Err(format!("parse_cose_key: EdDSA needs crv=Ed25519, got {crv:?}"));
            }
            let xb = x.ok_or_else(|| "EdDSA: missing x".to_string())?;
            if xb.len() != 32 {
                return Err("EdDSA: x must be 32 bytes".into());
            }
            let mut xa = [0u8; 32];
            xa.copy_from_slice(&xb);
            Ok((CoseKey::Ed25519 { x: xa }, consumed))
        }
        (3, -257) => {
            // For RSA the CBOR labels are -1 (n) and -2 (e).
            // We routed -1 bytes to rsa_n above and -2 bytes to `x`;
            // rsa_e gets the latter here.
            rsa_e = x;
            let n = rsa_n.ok_or_else(|| "RS256: missing n".to_string())?;
            let e = rsa_e.ok_or_else(|| "RS256: missing e".to_string())?;
            Ok((CoseKey::Rs256 { n, e }, consumed))
        }
        _ => Err(format!(
            "parse_cose_key: unsupported (kty={kty}, alg={alg}); supported: ES256, EdDSA, RS256"
        )),
    }
}

fn int_to_i64(i: ciborium::value::Integer) -> i64 {
    // ciborium::Integer round-trips through i128; clamp to i64 for the
    // tiny COSE label/alg space.
    let big: i128 = i.into();
    if big > i64::MAX as i128 {
        i64::MAX
    } else if big < i64::MIN as i128 {
        i64::MIN
    } else {
        big as i64
    }
}

// ─────────────── signature verification ───────────────

fn es256_verify(key: &CoseKey, data: &[u8], sig_der: &[u8]) -> bool {
    use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
    let (xb, yb) = match key {
        CoseKey::Es256 { x, y } => (x, y),
        _ => return false,
    };
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(xb);
    sec1.extend_from_slice(yb);
    let vk = match VerifyingKey::from_sec1_bytes(&sec1) {
        Ok(v) => v,
        Err(_) => return false,
    };
    // WebAuthn ES256 sigs are ASN.1 DER (the COSE_Sign1 raw r||s form
    // is only used inside CBOR-COSE; assertion sigs in WebAuthn ride
    // a plain ASN.1 SEQUENCE).
    let sig = match Signature::from_der(sig_der) {
        Ok(s) => s,
        Err(_) => return false,
    };
    vk.verify(data, &sig).is_ok()
}

fn ed25519_verify(key: &CoseKey, data: &[u8], sig: &[u8]) -> bool {
    use ed25519_dalek::Verifier;
    let xb = match key {
        CoseKey::Ed25519 { x } => x,
        _ => return false,
    };
    let vk = match ed25519_dalek::VerifyingKey::from_bytes(xb) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let sig_arr: [u8; 64] = match sig.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let s = ed25519_dalek::Signature::from_bytes(&sig_arr);
    vk.verify(data, &s).is_ok()
}

fn rs256_verify(key: &CoseKey, data: &[u8], sig: &[u8]) -> bool {
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier;
    use rsa::BigUint;
    let (n, e) = match key {
        CoseKey::Rs256 { n, e } => (n, e),
        _ => return false,
    };
    let n = BigUint::from_bytes_be(n);
    let e = BigUint::from_bytes_be(e);
    let pk = match rsa::RsaPublicKey::new(n, e) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let vk: VerifyingKey<sha2::Sha256> = VerifyingKey::new(pk);
    let sig = match Signature::try_from(sig) {
        Ok(s) => s,
        Err(_) => return false,
    };
    vk.verify(data, &sig).is_ok()
}

fn verify_with_cose(key: &CoseKey, data: &[u8], sig: &[u8]) -> bool {
    match key {
        CoseKey::Es256 { .. } => es256_verify(key, data, sig),
        CoseKey::Ed25519 { .. } => ed25519_verify(key, data, sig),
        CoseKey::Rs256 { .. } => rs256_verify(key, data, sig),
    }
}

// ─────────────── authenticator data parsing ───────────────
//
// WebAuthn § 6.1. Layout:
//   rpIdHash      32 bytes
//   flags          1 byte   bit 0 = UP, bit 2 = UV, bit 6 = AT, bit 7 = ED
//   signCount      4 bytes  big-endian
//   attestedCredentialData (present iff AT)
//       aaguid              16 bytes
//       credentialIdLength   2 bytes  big-endian
//       credentialId        L bytes
//       credentialPublicKey CBOR-encoded COSE_Key (variable length)
//   extensions    (present iff ED)

#[derive(Debug)]
pub struct AuthData {
    pub rp_id_hash: [u8; 32],
    pub flags: u8,
    pub sign_count: u32,
    pub attested: Option<AttestedCredentialData>,
}

#[derive(Debug)]
pub struct AttestedCredentialData {
    pub aaguid: [u8; 16],
    pub credential_id: Vec<u8>,
    pub credential_public_key: CoseKey,
}

impl AuthData {
    pub const FLAG_UP: u8 = 0b0000_0001;
    pub const FLAG_UV: u8 = 0b0000_0100;
    pub const FLAG_AT: u8 = 0b0100_0000;

    pub fn user_present(&self) -> bool {
        self.flags & Self::FLAG_UP != 0
    }
    pub fn user_verified(&self) -> bool {
        self.flags & Self::FLAG_UV != 0
    }
}

pub fn parse_authenticator_data(bytes: &[u8]) -> Result<AuthData, String> {
    if bytes.len() < 37 {
        return Err(format!("authData < 37 bytes (got {})", bytes.len()));
    }
    let mut rp = [0u8; 32];
    rp.copy_from_slice(&bytes[..32]);
    let flags = bytes[32];
    let sign_count = u32::from_be_bytes([bytes[33], bytes[34], bytes[35], bytes[36]]);
    let mut attested = None;
    if flags & AuthData::FLAG_AT != 0 {
        if bytes.len() < 37 + 18 {
            return Err("authData: AT bit set but no attestedCredentialData".into());
        }
        let mut aaguid = [0u8; 16];
        aaguid.copy_from_slice(&bytes[37..53]);
        let cid_len = u16::from_be_bytes([bytes[53], bytes[54]]) as usize;
        let cid_start = 55;
        let cid_end = cid_start + cid_len;
        if bytes.len() < cid_end {
            return Err("authData: credentialId truncated".into());
        }
        let credential_id = bytes[cid_start..cid_end].to_vec();
        let (cose, _consumed) = parse_cose_key(&bytes[cid_end..])?;
        attested = Some(AttestedCredentialData {
            aaguid,
            credential_id,
            credential_public_key: cose,
        });
    }
    Ok(AuthData {
        rp_id_hash: rp,
        flags,
        sign_count,
        attested,
    })
}

// ─────────────── clientDataJSON ───────────────

#[derive(Debug)]
pub struct ClientData {
    pub type_: String,
    pub challenge_b64: String,
    pub origin: String,
}

pub fn parse_client_data(bytes: &[u8]) -> Result<ClientData, String> {
    let s = core::str::from_utf8(bytes)
        .map_err(|e| format!("clientDataJSON UTF-8: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(s)
        .map_err(|e| format!("clientDataJSON parse: {e}"))?;
    let obj = v.as_object().ok_or_else(|| "clientDataJSON not an object".to_string())?;
    let type_ = obj.get("type").and_then(|x| x.as_str())
        .ok_or_else(|| "clientDataJSON: missing type".to_string())?.to_string();
    let challenge_b64 = obj.get("challenge").and_then(|x| x.as_str())
        .ok_or_else(|| "clientDataJSON: missing challenge".to_string())?.to_string();
    let origin = obj.get("origin").and_then(|x| x.as_str())
        .ok_or_else(|| "clientDataJSON: missing origin".to_string())?.to_string();
    Ok(ClientData { type_, challenge_b64, origin })
}

// ─────────────── attestationObject ───────────────

#[derive(Debug)]
pub struct AttestationObject {
    pub fmt: String,
    pub auth_data: Vec<u8>,
    pub att_stmt: ciborium::value::Value,
}

pub fn parse_attestation_object(bytes: &[u8]) -> Result<AttestationObject, String> {
    use ciborium::value::Value;
    let v: Value = ciborium::de::from_reader(bytes)
        .map_err(|e| format!("attestationObject CBOR: {e:?}"))?;
    let map = match v {
        Value::Map(m) => m,
        _ => return Err("attestationObject: expected map".into()),
    };
    let mut fmt: Option<String> = None;
    let mut auth_data: Option<Vec<u8>> = None;
    let mut att_stmt: Option<Value> = None;
    for (k, v) in map {
        let key = match k {
            Value::Text(s) => s,
            _ => continue,
        };
        match key.as_str() {
            "fmt" => fmt = v.as_text().map(|s| s.to_string()),
            "authData" => auth_data = v.as_bytes().cloned(),
            "attStmt" => att_stmt = Some(v),
            _ => {}
        }
    }
    Ok(AttestationObject {
        fmt: fmt.ok_or_else(|| "attestationObject: missing fmt".to_string())?,
        auth_data: auth_data
            .ok_or_else(|| "attestationObject: missing authData".to_string())?,
        att_stmt: att_stmt.unwrap_or(Value::Map(alloc::vec::Vec::new())),
    })
}

// ─────────────── challenge generation ───────────────

fn random_challenge() -> Result<Vec<u8>, String> {
    // 32 bytes per WebAuthn § 13.1 guidance (>= 16 required; 32+
    // recommended for collision resistance against multi-second
    // ceremonies).
    let mut buf = alloc::vec![0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| format!("getrandom: {e}"))?;
    Ok(buf)
}

// ─────────────── options builders ───────────────

pub fn register_options(
    rp_id: &str,
    rp_name: &str,
    user_id: &[u8],
    user_name: &str,
    user_display_name: &str,
) -> Result<String, String> {
    let challenge = random_challenge()?;
    let mut out = serde_json::Map::new();
    out.insert("challenge".into(), serde_json::Value::String(b64url_encode(&challenge)));
    let mut rp = serde_json::Map::new();
    rp.insert("id".into(), serde_json::Value::String(rp_id.into()));
    rp.insert("name".into(), serde_json::Value::String(rp_name.into()));
    out.insert("rp".into(), serde_json::Value::Object(rp));
    let mut user = serde_json::Map::new();
    user.insert("id".into(), serde_json::Value::String(b64url_encode(user_id)));
    user.insert("name".into(), serde_json::Value::String(user_name.into()));
    user.insert(
        "displayName".into(),
        serde_json::Value::String(user_display_name.into()),
    );
    out.insert("user".into(), serde_json::Value::Object(user));
    // pubKeyCredParams: offer the three algs we can actually verify
    // (ES256 first; RS256 last because keys are large).
    let pkcp = alloc::vec![
        cred_param(-7),
        cred_param(-8),
        cred_param(-257),
    ];
    out.insert("pubKeyCredParams".into(), serde_json::Value::Array(pkcp));
    out.insert("timeout".into(), serde_json::Value::Number(60000.into()));
    out.insert("attestation".into(), serde_json::Value::String("none".into()));
    Ok(serde_json::Value::Object(out).to_string())
}

fn cred_param(alg_id: i64) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), serde_json::Value::String("public-key".into()));
    m.insert("alg".into(), serde_json::Value::Number(alg_id.into()));
    serde_json::Value::Object(m)
}

pub fn auth_options(
    rp_id: &str,
    allow_credentials_json: &str,
    user_verification: Option<&str>,
) -> Result<String, String> {
    let challenge = random_challenge()?;
    let mut out = serde_json::Map::new();
    out.insert("challenge".into(), serde_json::Value::String(b64url_encode(&challenge)));
    out.insert("rpId".into(), serde_json::Value::String(rp_id.into()));
    let allow: serde_json::Value = serde_json::from_str(allow_credentials_json)
        .map_err(|e| format!("auth_options: allow_credentials JSON: {e}"))?;
    if !allow.is_array() {
        return Err("auth_options: allow_credentials must be a JSON array".into());
    }
    out.insert("allowCredentials".into(), allow);
    out.insert("timeout".into(), serde_json::Value::Number(60000.into()));
    out.insert(
        "userVerification".into(),
        serde_json::Value::String(user_verification.unwrap_or("preferred").into()),
    );
    Ok(serde_json::Value::Object(out).to_string())
}

// ─────────────── verify helpers ───────────────

fn get_b64_string(
    obj: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<u8>, String> {
    let s = obj.get(field).and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing field {field}"))?;
    b64url_decode(s)
}

fn get_options_challenge_b64(opts: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(opts)
        .map_err(|e| format!("options JSON: {e}"))?;
    let s = v.get("challenge").and_then(|x| x.as_str())
        .ok_or_else(|| "options: missing challenge".to_string())?;
    Ok(s.to_string())
}

// ─────────────── verify_registration ───────────────

pub fn verify_registration(
    options_json: &str,
    response_json: &str,
    expected_rp_id: &str,
) -> Option<String> {
    verify_registration_inner(options_json, response_json, expected_rp_id).ok()
}

fn verify_registration_inner(
    options_json: &str,
    response_json: &str,
    expected_rp_id: &str,
) -> Result<String, String> {
    let opt_challenge_b64 = get_options_challenge_b64(options_json)?;
    let opts_v: serde_json::Value = serde_json::from_str(options_json)
        .map_err(|e| format!("options JSON: {e}"))?;
    let user_id_b64 = opts_v.pointer("/user/id").and_then(|v| v.as_str())
        .ok_or_else(|| "options: missing /user/id".to_string())?
        .to_string();

    let resp_v: serde_json::Value = serde_json::from_str(response_json)
        .map_err(|e| format!("response JSON: {e}"))?;
    let resp_obj = resp_v.as_object()
        .ok_or_else(|| "response: not an object".to_string())?;
    let credential_id_b64 = resp_obj.get("id").and_then(|v| v.as_str())
        .or_else(|| resp_obj.get("rawId").and_then(|v| v.as_str()))
        .ok_or_else(|| "response: missing id/rawId".to_string())?
        .to_string();

    let inner = resp_obj.get("response").and_then(|v| v.as_object())
        .ok_or_else(|| "response: missing .response object".to_string())?;
    let client_data_bytes = get_b64_string(inner, "clientDataJSON")?;
    let att_obj_bytes = get_b64_string(inner, "attestationObject")?;

    let cdata = parse_client_data(&client_data_bytes)?;
    if cdata.type_ != "webauthn.create" {
        return Err(format!(
            "clientDataJSON.type {:?}, expected webauthn.create",
            cdata.type_
        ));
    }
    if cdata.challenge_b64 != opt_challenge_b64 {
        return Err("clientDataJSON.challenge != options.challenge".into());
    }
    if cdata.origin.is_empty() || cdata.origin == "null" {
        return Err("clientDataJSON.origin empty/null".into());
    }

    let att = parse_attestation_object(&att_obj_bytes)?;
    let auth_data = parse_authenticator_data(&att.auth_data)?;
    let expected = sha256(expected_rp_id.as_bytes());
    if auth_data.rp_id_hash != expected {
        return Err("authData.rpIdHash != sha256(expected_rp_id)".into());
    }
    if !auth_data.user_present() {
        return Err("authData.flags.UP not set".into());
    }
    let attested = auth_data.attested.as_ref()
        .ok_or_else(|| "authData: AT flag not set, no credential".to_string())?;
    match att.fmt.as_str() {
        "none" => {
            // No statement to verify. Spec § 8.7.
        }
        "packed" => {
            // Self-attestation only. With x5c we'd need to walk the
            // cert chain to a trusted anchor; explicitly reject so
            // we don't silently accept an unvalidated chain.
            if let ciborium::value::Value::Map(ref m) = att.att_stmt {
                let has_x5c = m.iter().any(|(k, _)| {
                    matches!(k, ciborium::value::Value::Text(s) if s == "x5c")
                });
                if has_x5c {
                    return Err(
                        "packed attestation with x5c: chain validation is deferred; \
                         pair with an out-of-band cert-trust pipeline".into()
                    );
                }
                let sig = m.iter().find_map(|(k, v)| match (k, v) {
                    (ciborium::value::Value::Text(s), ciborium::value::Value::Bytes(b))
                        if s == "sig" => Some(b.clone()),
                    _ => None,
                });
                let sig = sig.ok_or_else(|| "packed attestation: missing sig".to_string())?;
                let mut tbs = att.auth_data.clone();
                tbs.extend_from_slice(&sha256(&client_data_bytes));
                if !verify_with_cose(&attested.credential_public_key, &tbs, &sig) {
                    return Err("packed attestation: self-sig verify failed".into());
                }
            } else {
                return Err("packed attestation: attStmt not a CBOR map".into());
            }
        }
        other => {
            return Err(format!(
                "attestation fmt {other:?} not implemented (none + packed-self only)"
            ));
        }
    }

    let mut out = serde_json::Map::new();
    out.insert("user_id".into(), serde_json::Value::String(user_id_b64));
    out.insert(
        "credential_id".into(),
        serde_json::Value::String(b64url_encode(&attested.credential_id)),
    );
    out.insert(
        "credential_id_response".into(),
        serde_json::Value::String(credential_id_b64),
    );
    out.insert(
        "public_key".into(),
        serde_json::Value::String(attested.credential_public_key.to_json()),
    );
    out.insert(
        "sign_count".into(),
        serde_json::Value::Number((auth_data.sign_count as i64).into()),
    );
    out.insert(
        "attestation_format".into(),
        serde_json::Value::String(att.fmt.clone()),
    );
    out.insert(
        "aaguid".into(),
        serde_json::Value::String(hex::encode(attested.aaguid)),
    );
    out.insert(
        "user_verified".into(),
        serde_json::Value::Bool(auth_data.user_verified()),
    );
    Ok(serde_json::Value::Object(out).to_string())
}

// ─────────────── verify_authentication ───────────────

pub fn verify_authentication(
    options_json: &str,
    response_json: &str,
    credential_pub_key_json: &str,
    expected_sign_count: u32,
) -> Option<String> {
    verify_authentication_inner(
        options_json,
        response_json,
        credential_pub_key_json,
        expected_sign_count,
    )
    .ok()
}

fn verify_authentication_inner(
    options_json: &str,
    response_json: &str,
    credential_pub_key_json: &str,
    expected_sign_count: u32,
) -> Result<String, String> {
    let opt_challenge_b64 = get_options_challenge_b64(options_json)?;
    let opts_v: serde_json::Value = serde_json::from_str(options_json)
        .map_err(|e| format!("options JSON: {e}"))?;
    let rp_id = opts_v.get("rpId").and_then(|v| v.as_str())
        .ok_or_else(|| "options: missing rpId".to_string())?;

    let resp_v: serde_json::Value = serde_json::from_str(response_json)
        .map_err(|e| format!("response JSON: {e}"))?;
    let resp_obj = resp_v.as_object()
        .ok_or_else(|| "response: not an object".to_string())?;
    let inner = resp_obj.get("response").and_then(|v| v.as_object())
        .ok_or_else(|| "response: missing .response object".to_string())?;
    let client_data_bytes = get_b64_string(inner, "clientDataJSON")?;
    let auth_data_bytes = get_b64_string(inner, "authenticatorData")?;
    let signature = get_b64_string(inner, "signature")?;

    let cdata = parse_client_data(&client_data_bytes)?;
    if cdata.type_ != "webauthn.get" {
        return Err(format!(
            "clientDataJSON.type {:?}, expected webauthn.get",
            cdata.type_
        ));
    }
    if cdata.challenge_b64 != opt_challenge_b64 {
        return Err("clientDataJSON.challenge != options.challenge".into());
    }
    if cdata.origin.is_empty() || cdata.origin == "null" {
        return Err("clientDataJSON.origin empty/null".into());
    }

    let auth_data = parse_authenticator_data(&auth_data_bytes)?;
    let expected = sha256(rp_id.as_bytes());
    if auth_data.rp_id_hash != expected {
        return Err("authData.rpIdHash != sha256(rpId)".into());
    }
    if !auth_data.user_present() {
        return Err("authData.flags.UP not set".into());
    }
    let key = CoseKey::from_json(credential_pub_key_json)?;
    let mut tbs = auth_data_bytes.clone();
    tbs.extend_from_slice(&sha256(&client_data_bytes));
    if !verify_with_cose(&key, &tbs, &signature) {
        return Err("assertion signature verify failed".into());
    }
    // Sign-count anti-cloning: WebAuthn § 7.2 step 17. If both old &
    // new are zero the authenticator doesn't implement counters
    // accept. Else new MUST be strictly greater than old.
    if (auth_data.sign_count != 0 || expected_sign_count != 0)
        && auth_data.sign_count <= expected_sign_count
    {
        return Err(format!(
            "sign_count regression: got {}, expected > {}",
            auth_data.sign_count, expected_sign_count
        ));
    }

    let mut out = serde_json::Map::new();
    out.insert(
        "new_sign_count".into(),
        serde_json::Value::Number((auth_data.sign_count as i64).into()),
    );
    out.insert(
        "user_verified".into(),
        serde_json::Value::Bool(auth_data.user_verified()),
    );
    Ok(serde_json::Value::Object(out).to_string())
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

    const FID_REGISTER_OPTS: u64 = 1;
    const FID_VERIFY_REG: u64 = 2;
    const FID_AUTH_OPTS: u64 = 3;
    const FID_VERIFY_AUTH: u64 = 4;
    const FID_VERSION: u64 = 5;
    const FID_AUTH_OPTS_3: u64 = 6;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_text_opt(args: &[SqlValue], i: usize) -> Option<String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            _ => None,
        }
    }

    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // register/auth options pull fresh challenge bytes from
            // getrandom; must NOT be flagged deterministic or the
            // planner will memoize them.
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "webauthn".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_REGISTER_OPTS, "webauthn_register_options", 5, nd),
                    s(FID_VERIFY_REG,    "webauthn_verify_registration", 3, det),
                    // Two-arity registrations for auth_options so
                    // callers can pass the optional userVerification
                    // override without juggling default args.
                    s(FID_AUTH_OPTS,     "webauthn_auth_options", 2, nd),
                    s(FID_AUTH_OPTS_3,   "webauthn_auth_options", 3, nd),
                    s(FID_VERIFY_AUTH,   "webauthn_verify_authentication", 4, det),
                    s(FID_VERSION,       "webauthn_version", 0, det),
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
                preferred_prefix: Some("webauthn".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.webauthn".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_REGISTER_OPTS => {
                    let rp_id = arg_text(&args, 0, "webauthn_register_options")?;
                    let rp_name = arg_text(&args, 1, "webauthn_register_options")?;
                    let user_id = arg_blob(&args, 2, "webauthn_register_options")?;
                    let user_name = arg_text(&args, 3, "webauthn_register_options")?;
                    let display = arg_text(&args, 4, "webauthn_register_options")?;
                    super::register_options(&rp_id, &rp_name, &user_id, &user_name, &display)
                        .map(SqlValue::Text)
                }
                FID_VERIFY_REG => {
                    let opts = arg_text(&args, 0, "webauthn_verify_registration")?;
                    let resp = arg_text(&args, 1, "webauthn_verify_registration")?;
                    let rp = arg_text(&args, 2, "webauthn_verify_registration")?;
                    Ok(match super::verify_registration(&opts, &resp, &rp) {
                        Some(s) => SqlValue::Text(s),
                        None => SqlValue::Null,
                    })
                }
                FID_AUTH_OPTS | FID_AUTH_OPTS_3 => {
                    let rp_id = arg_text(&args, 0, "webauthn_auth_options")?;
                    let allow = arg_text(&args, 1, "webauthn_auth_options")?;
                    let uv = arg_text_opt(&args, 2);
                    super::auth_options(&rp_id, &allow, uv.as_deref()).map(SqlValue::Text)
                }
                FID_VERIFY_AUTH => {
                    let opts = arg_text(&args, 0, "webauthn_verify_authentication")?;
                    let resp = arg_text(&args, 1, "webauthn_verify_authentication")?;
                    let key = arg_text(&args, 2, "webauthn_verify_authentication")?;
                    let count = arg_int(&args, 3, "webauthn_verify_authentication")?;
                    let count: u32 = if count < 0 { 0 } else { count as u32 };
                    Ok(
                        match super::verify_authentication(&opts, &resp, &key, count) {
                            Some(s) => SqlValue::Text(s),
                            None => SqlValue::Null,
                        },
                    )
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("webauthn: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
