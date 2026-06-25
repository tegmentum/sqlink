//! OAuth 2.0 PKCE (Proof Key for Code Exchange, RFC 7636) helpers.
//!
//! The "auth-helpers" family alongside `jwt` (token verification),
//! `totp` (second factor), and `pwhash` (password storage). PKCE is
//! how public OAuth clients (mobile, SPA, CLI) bind an authorization
//! code to the same caller that initiated the flow  the standard
//! defense against authorization-code interception.
//!
//! Function surface:
//!
//!   pkce_verifier([byte_len])      -> TEXT (43..128 chars, RFC 7636 §4.1)
//!   pkce_challenge_s256(verifier)  -> TEXT (base64url-no-pad of SHA-256(verifier))
//!   pkce_challenge_plain(verifier) -> TEXT (the verifier itself, §4.2)
//!   pkce_version()                 -> TEXT
//!
//! The verifier ABNF (RFC 7636 §4.1):
//!   code-verifier = 43*128unreserved
//!   unreserved    = ALPHA / DIGIT / "-" / "." / "_" / "~"
//!
//! We generate the verifier by drawing `byte_len` random bytes
//! (default 32 = 256 bits) and base64url-encoding them with no
//! padding. byte_len is clamped 32..=96, mapping to 43..=128
//! base64url chars per the RFC. 32 is the recommended floor (well
//! above 128 bits of entropy); 96 is the upper bound that fits in
//! the 128-char ceiling.
//!
//! The S256 transform is BASE64URL(SHA256(ASCII(verifier))) with
//! no padding, per RFC 7636 §4.2.
//!
//! Acceptance vector  RFC 7636 §4.6 / Appendix B:
//!   verifier  = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
//!   challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
//!
//! `plain` is included for spec completeness (RFC 7636 §4.4 method
//! "plain") but should NOT be used in production  S256 is the only
//! method modern authorization servers should accept (RFC 7636 §7.2
//! is explicit that plain is downgrade-attack-prone).

extern crate alloc;

use alloc::string::String;

use sha2::{Digest, Sha256};

// ─────────────── base64url (RFC 4648 §5, no padding) ───────────────

/// base64url alphabet per RFC 4648 §5 / RFC 7636 §4.1
/// ("base64url-encoded" => URL-safe alphabet, no padding).
const B64URL: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url-encode `bytes` with NO padding. This is the encoding
/// PKCE wants  the verifier ABNF only permits A-Z / a-z / 0-9 /
/// '-' / '.' / '_' / '~' but PKCE itself only emits the base64url
/// subset (A-Z / a-z / 0-9 / '-' / '_') so '.' and '~' never
/// appear in a verifier we generate, just in user-supplied ones
/// (which we treat as opaque  we only hash, never re-encode).
pub fn b64url_encode_nopad(bytes: &[u8]) -> String {
    // Each 3 input bytes  4 output chars; tail of 1/2 bytes
    // 2/3 chars (no '=' padding).
    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let b0 = bytes[i] as u32;
        let b1 = bytes[i + 1] as u32;
        let b2 = bytes[i + 2] as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64URL[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64URL[((n >> 6) & 0x3f) as usize] as char);
        out.push(B64URL[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let b0 = bytes[i] as u32;
        let n = b0 << 16;
        out.push(B64URL[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64URL[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        let b0 = bytes[i] as u32;
        let b1 = bytes[i + 1] as u32;
        let n = (b0 << 16) | (b1 << 8);
        out.push(B64URL[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64URL[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64URL[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

// ─────────────── PKCE core ───────────────

/// Generate a fresh code verifier per RFC 7636 §4.1.
///
/// `byte_len` is the number of raw random bytes to draw before
/// base64url encoding; clamped to 32..=96 so the emitted string
/// length is 43..=128 (matching the §4.1 ABNF). 32 bytes = 256
/// bits is the recommended size  the RFC's 256-bit floor with
/// margin; 96 maps to the spec's 128-char ceiling.
///
/// The encoded form is RFC 4648 §5 base64url with no padding
/// only contains characters from the unreserved set, so it's a
/// well-formed verifier without further escaping.
pub fn random_verifier(byte_len: u32) -> Result<String, String> {
    if !(32..=96).contains(&byte_len) {
        return Err(alloc::format!(
            "pkce_verifier: byte_len must be 32..=96 (43..=128 encoded), got {byte_len}"
        ));
    }
    let mut buf = alloc::vec![0u8; byte_len as usize];
    getrandom::getrandom(&mut buf).map_err(|e| alloc::format!("pkce_verifier: {e}"))?;
    Ok(b64url_encode_nopad(&buf))
}

/// Compute the S256 code challenge per RFC 7636 §4.2:
///
///   code_challenge = BASE64URL(SHA256(ASCII(code_verifier)))
///
/// "ASCII" in the RFC just means "the bytes of the verifier
/// string"  the verifier ABNF is ASCII-only by construction, so
/// the UTF-8 byte view is identical. The output is base64url with
/// no padding, 43 chars (SHA-256 is 32 bytes  43.0 base64url chars
/// rounded up to 43 with no pad).
pub fn challenge_s256(verifier: &str) -> String {
    let mut h = Sha256::new();
    h.update(verifier.as_bytes());
    let digest = h.finalize();
    b64url_encode_nopad(&digest)
}

/// "plain" challenge method per RFC 7636 §4.4. The challenge is
/// literally the verifier. Included for spec completeness; modern
/// servers should reject this method (RFC 7636 §7.2 explicitly
/// warns it's downgrade-attack-prone). Exposed so SQL can express
/// "compute the challenge for any method the metadata says is in
/// play" without branching outside SQL.
pub fn challenge_plain(verifier: &str) -> String {
    verifier.into()
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

    // FID layout: pkce_verifier has the 0-arg + 1-arg overload, the
    // rest are fixed-arity.
    const FID_VERIFIER_0: u64 = 1;
    const FID_VERIFIER_1: u64 = 2;
    const FID_CHALLENGE_S256: u64 = 3;
    const FID_CHALLENGE_PLAIN: u64 = 4;
    const FID_VERSION: u64 = 5;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            // Permit BLOB for the challenge inputs  some callers
            // store verifiers as raw bytes (e.g. session-blob columns).
            // SHA-256 doesn't care which it sees.
            Some(SqlValue::Blob(b)) => core::str::from_utf8(b)
                .map(|s| s.to_string())
                .map_err(|e| format!("{fname}: arg {i} BLOB not utf-8: {e}")),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_u32(args: &[SqlValue], i: usize, fname: &str) -> Result<u32, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) if *n >= 0 && *n <= u32::MAX as i64 => Ok(*n as u32),
            _ => Err(format!("{fname}: non-negative INTEGER arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // `pkce_verifier` draws randomness and so cannot be
            // marked DETERMINISTIC (SQLite would cache calls and
            // every row in a SELECT would get the same verifier
            // exactly the wrong behavior for "generate a fresh one
            // per row"). The challenge transforms are pure functions
            // of their input, so they ARE deterministic.
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: f,
            };
            Manifest {
                name: "oauth_pkce".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VERIFIER_0, "pkce_verifier", 0, nd),
                    s(FID_VERIFIER_1, "pkce_verifier", 1, nd),
                    s(FID_CHALLENGE_S256, "pkce_challenge_s256", 1, det),
                    s(FID_CHALLENGE_PLAIN, "pkce_challenge_plain", 1, det),
                    s(FID_VERSION, "pkce_version", 0, det),
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
                preferred_prefix: Some("oauth_pkce".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.oauth_pkce".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERIFIER_0 => super::random_verifier(32).map(SqlValue::Text),
                FID_VERIFIER_1 => {
                    let n = arg_u32(&args, 0, "pkce_verifier")?;
                    super::random_verifier(n).map(SqlValue::Text)
                }
                FID_CHALLENGE_S256 => {
                    let v = arg_text(&args, 0, "pkce_challenge_s256")?;
                    Ok(SqlValue::Text(super::challenge_s256(&v)))
                }
                FID_CHALLENGE_PLAIN => {
                    let v = arg_text(&args, 0, "pkce_challenge_plain")?;
                    Ok(SqlValue::Text(super::challenge_plain(&v)))
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "oauth-pkce {} (RFC 7636)",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("oauth-pkce: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
