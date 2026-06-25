//! TOTP (RFC 6238) + HOTP (RFC 4226) scalars.
//!
//! Companion to the `jwt` (token) + `pwhash` (password) + `aead`
//! (vault) extensions; this one is the 2-factor half.
//!
//! Function surface (PLAN-more-extensions-2.md  1):
//!
//!   totp_generate(secret_b32, [period_s], [digits], [algorithm]) -> TEXT
//!   totp_verify(code, secret_b32, [period_s], [digits], [algorithm], [window]) -> INTEGER
//!   hotp_generate(secret_b32, counter, [digits], [algorithm]) -> TEXT
//!   hotp_verify(code, secret_b32, counter, [digits], [algorithm]) -> INTEGER
//!   totp_url(label, secret_b32, [issuer], [period_s], [digits], [algorithm]) -> TEXT
//!   totp_secret([byte_len]) -> TEXT (base32, default 20 bytes)
//!   totp_now() -> INTEGER (current epoch seconds)
//!   totp_version() -> TEXT
//!
//! Defaults match the RFC 6238 baseline + what authenticator apps
//! assume: period_s=30, digits=6, algorithm='SHA1', window=1
//! (accept code from ±1 step on verify, so a code generated up to
//! `period_s` early or late still verifies  covers clock skew).
//!
//! Implementation note: rather than depend on `totp-rs` we roll the
//! RFC 4226 dynamic-truncation step (~20 lines) over `hmac` + `sha1`
//! / `sha2`. Same trade-off as the `jwt` extension's hand-rolled
//! JOSE  smaller binary, no transient deps (totp-rs pulls qrcode).
//!
//! All `*_verify` functions return INTEGER 0 on any failure mode
//! (bad base32, malformed code, unknown algorithm) so callers can
//! drop them into WHERE / CASE without try/catch.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use hmac::{Hmac, Mac};

// ─────────────── algorithm ───────────────

/// The three HMAC-SHA variants RFC 6238 lists. SHA1 is the
/// historic baseline (every authenticator app speaks it); SHA256
/// and SHA512 are the strengthened options some enterprise IdPs
/// emit. Case is normalized at the parse boundary so callers can
/// pass 'sha1', 'SHA1', 'Sha1' interchangeably.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alg {
    Sha1,
    Sha256,
    Sha512,
}

impl Alg {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_uppercase().as_str() {
            "SHA1" | "SHA-1" => Ok(Alg::Sha1),
            "SHA256" | "SHA-256" => Ok(Alg::Sha256),
            "SHA512" | "SHA-512" => Ok(Alg::Sha512),
            _ => Err(format!("unsupported algorithm: {s:?}")),
        }
    }

    /// otpauth:// URL `algorithm=` parameter; matches Google
    /// Authenticator Key URI spec.
    pub fn url_name(self) -> &'static str {
        match self {
            Alg::Sha1 => "SHA1",
            Alg::Sha256 => "SHA256",
            Alg::Sha512 => "SHA512",
        }
    }
}

// ─────────────── base32 helpers ───────────────

/// Base32 alphabet: RFC 4648 (the Google Authenticator / authenticator
/// app convention). We accept padded and unpadded inputs alike, and
/// normalize case so callers can paste either '====' suffix or no
/// suffix interchangeably.
fn b32_decode(secret: &str) -> Result<Vec<u8>, String> {
    // Strip whitespace common in QR-rendered secrets ("ABCD EFGH").
    let cleaned: String = secret.chars().filter(|c| !c.is_whitespace()).collect();
    let upper = cleaned.to_ascii_uppercase();
    base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &upper)
        .or_else(|| base32::decode(base32::Alphabet::Rfc4648 { padding: true }, &upper))
        .ok_or_else(|| "invalid base32 secret".to_string())
}

fn b32_encode(bytes: &[u8]) -> String {
    // No padding: matches what otpauth:// URLs / Google Authenticator
    // expect. The decode side above accepts padded too in case a
    // caller pastes one back in.
    base32::encode(base32::Alphabet::Rfc4648 { padding: false }, bytes)
}

// ─────────────── HOTP core (RFC 4226) ───────────────

/// HOTP = Truncate( HMAC-SHA(key, counter_be8) ) mod 10^digits.
///
/// "Truncate" is the dynamic-truncation step in RFC 4226 §5.3:
///   offset = mac[mac.len()-1] & 0x0f
///   take 4 bytes starting at offset, big-endian
///   mask off the high bit (so the result is positive on signed
///   32-bit  the RFC was written when 32-bit signed ints were
///   the lingua franca of compatibility libraries)
///   modulo 10^digits to get a `digits`-digit decimal
///
/// `digits` is clamped 6..=10  RFC 4226 specifies 6..8 but
/// some authenticator apps go up to 10; below 6 leaks bits, above
/// 10 overflows u32 (max code 4294967295 < 10^10).
pub fn hotp(secret: &[u8], counter: u64, digits: u32, alg: Alg) -> Result<String, String> {
    if !(6..=10).contains(&digits) {
        return Err(format!("hotp: digits must be 6..=10, got {digits}"));
    }
    let counter_be = counter.to_be_bytes();
    let mac: Vec<u8> = match alg {
        Alg::Sha1 => {
            let mut m = <Hmac<sha1::Sha1>>::new_from_slice(secret)
                .map_err(|e| format!("hotp: hmac key: {e}"))?;
            m.update(&counter_be);
            m.finalize().into_bytes().to_vec()
        }
        Alg::Sha256 => {
            let mut m = <Hmac<sha2::Sha256>>::new_from_slice(secret)
                .map_err(|e| format!("hotp: hmac key: {e}"))?;
            m.update(&counter_be);
            m.finalize().into_bytes().to_vec()
        }
        Alg::Sha512 => {
            let mut m = <Hmac<sha2::Sha512>>::new_from_slice(secret)
                .map_err(|e| format!("hotp: hmac key: {e}"))?;
            m.update(&counter_be);
            m.finalize().into_bytes().to_vec()
        }
    };
    // Dynamic truncation per RFC 4226 §5.3
    let offset = (mac[mac.len() - 1] & 0x0f) as usize;
    let bin_code = ((mac[offset] as u32 & 0x7f) << 24)
        | ((mac[offset + 1] as u32) << 16)
        | ((mac[offset + 2] as u32) << 8)
        | (mac[offset + 3] as u32);
    let modulus = 10u32.pow(digits);
    let code = bin_code % modulus;
    // Zero-pad to `digits` width  authenticator apps render
    // "094281", not "94281".
    Ok(format!("{code:0width$}", width = digits as usize))
}

/// HOTP verify: constant-time compare to avoid a timing leak on the
/// number-of-leading-matching-digits. Returns false on any error path
/// (bad base32, bad alg, anything)  callers use the result directly
/// in WHERE / CASE.
pub fn hotp_verify(
    code: &str,
    secret: &[u8],
    counter: u64,
    digits: u32,
    alg: Alg,
) -> bool {
    let Ok(expected) = hotp(secret, counter, digits, alg) else {
        return false;
    };
    ct_eq(code.as_bytes(), expected.as_bytes())
}

/// Constant-time byte slice equality. The `subtle` crate would do
/// this too but we already have one HMAC dep, no need for another
/// micro-crate. `expected` length is fixed per (digits) so a leak
/// on length alone is fine.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ─────────────── TOTP core (RFC 6238) ───────────────

/// TOTP at a specific UNIX-epoch second `now`. The counter is
/// floor(now / period_s) per RFC 6238 §4.2. Period must be > 0;
/// 30 is the universal default.
pub fn totp_at(
    secret: &[u8],
    now_secs: u64,
    period_s: u32,
    digits: u32,
    alg: Alg,
) -> Result<String, String> {
    if period_s == 0 {
        return Err("totp: period_s must be > 0".into());
    }
    let counter = now_secs / period_s as u64;
    hotp(secret, counter, digits, alg)
}

/// TOTP verify with a sliding window. window=0 = exact step, window=1
/// = accept the current step and the steps immediately before / after
/// (so a code generated up to period_s early or late still verifies).
/// window=2 = ±2 steps, and so on. Larger windows are weaker (more
/// codes valid at once); RFC 6238 §5.2 recommends 1.
///
/// Returns false on any error  bad base32, bad alg, malformed code.
pub fn totp_verify_at(
    code: &str,
    secret: &[u8],
    now_secs: u64,
    period_s: u32,
    digits: u32,
    alg: Alg,
    window: u32,
) -> bool {
    if period_s == 0 {
        return false;
    }
    let center = now_secs / period_s as u64;
    let w = window as i64;
    for delta in -w..=w {
        let c = match (center as i64).checked_add(delta) {
            Some(v) if v >= 0 => v as u64,
            _ => continue,
        };
        if let Ok(expected) = hotp(secret, c, digits, alg) {
            if ct_eq(code.as_bytes(), expected.as_bytes()) {
                return true;
            }
        }
    }
    false
}

// ─────────────── otpauth:// URL builder ───────────────

/// Google Authenticator Key URI spec
/// (https://github.com/google/google-authenticator/wiki/Key-Uri-Format):
///
///   otpauth://totp/<label>?secret=<b32>&issuer=<x>&algorithm=<x>
///                         &digits=<n>&period=<s>
///
/// `label` is typically "Issuer:account@example.com". We percent-encode
/// the conservative set RFC 3986 §2.2 calls reserved + space  some
/// authenticator apps choke on raw spaces / colons. The `issuer` query
/// parameter is redundant with the label prefix per spec but apps tend
/// to render whichever they parse first, so we emit both when issuer
/// is given.
pub fn totp_url(
    label: &str,
    secret: &[u8],
    issuer: Option<&str>,
    period_s: u32,
    digits: u32,
    alg: Alg,
) -> String {
    let secret_b32 = b32_encode(secret);
    let mut url = String::with_capacity(128);
    url.push_str("otpauth://totp/");
    // Embed the issuer prefix in the label if both are given AND the
    // label doesn't already contain a colon. The spec recommends
    // "Issuer:account" and the issuer= param both.
    let label_full = match issuer {
        Some(iss) if !label.contains(':') => format!("{iss}:{label}"),
        _ => label.to_string(),
    };
    url.push_str(&pct_encode_path(&label_full));
    url.push_str("?secret=");
    url.push_str(&secret_b32);
    if let Some(iss) = issuer {
        url.push_str("&issuer=");
        url.push_str(&pct_encode_query(iss));
    }
    url.push_str("&algorithm=");
    url.push_str(alg.url_name());
    url.push_str(&format!("&digits={digits}"));
    url.push_str(&format!("&period={period_s}"));
    url
}

/// Percent-encode the path segment after `otpauth://totp/`. We keep
/// `:` because it's part of the canonical "Issuer:account" form  some
/// apps look for it specifically. Spaces and other reserved chars get
/// encoded.
fn pct_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        // Unreserved per RFC 3986 §2.3 + ':' (path-segment-safe per
        // §3.3) + '@' for the email-style account suffix.
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b':' | b'@') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-encode a query-string value. Stricter than path: no `:`,
/// no `@`, no `/`  these matter inside `?issuer=...&...`.
fn pct_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ─────────────── secret generation ───────────────

/// Generate `byte_len` random bytes (default 20 = 160 bits, the RFC
/// 4226 recommended minimum for HOTP). Returned as RFC 4648 base32
/// without padding  the same shape `totp_generate` accepts.
///
/// byte_len is clamped 16..=64. 16 bytes = 128 bits is the bare
/// minimum some apps still accept (legacy 80-bit secrets exist but
/// are below modern recommendation); 64 bytes is the SHA-512 block
/// size, above which there's no extra security from HMAC's
/// pre-hashing step.
pub fn random_secret(byte_len: u32) -> Result<String, String> {
    if !(16..=64).contains(&byte_len) {
        return Err(format!(
            "totp_secret: byte_len must be 16..=64, got {byte_len}"
        ));
    }
    let mut buf = alloc::vec![0u8; byte_len as usize];
    getrandom::getrandom(&mut buf).map_err(|e| format!("totp_secret: {e}"))?;
    Ok(b32_encode(&buf))
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

    // FID layout: one per (fname, arity). The arity-overloaded
    // families (totp_generate has 1..=4 args, totp_verify 2..=6,
    // etc.) need a slot each so the dispatcher can route on FID.
    // Keep this aligned with the describe() table below.
    const FID_TOTP_GENERATE_1: u64 = 1;
    const FID_TOTP_GENERATE_2: u64 = 2;
    const FID_TOTP_GENERATE_3: u64 = 3;
    const FID_TOTP_GENERATE_4: u64 = 4;

    const FID_TOTP_VERIFY_2: u64 = 10;
    const FID_TOTP_VERIFY_3: u64 = 11;
    const FID_TOTP_VERIFY_4: u64 = 12;
    const FID_TOTP_VERIFY_5: u64 = 13;
    const FID_TOTP_VERIFY_6: u64 = 14;

    const FID_HOTP_GENERATE_2: u64 = 20;
    const FID_HOTP_GENERATE_3: u64 = 21;
    const FID_HOTP_GENERATE_4: u64 = 22;

    const FID_HOTP_VERIFY_3: u64 = 30;
    const FID_HOTP_VERIFY_4: u64 = 31;
    const FID_HOTP_VERIFY_5: u64 = 32;

    const FID_TOTP_URL_2: u64 = 40;
    const FID_TOTP_URL_3: u64 = 41;
    const FID_TOTP_URL_4: u64 = 42;
    const FID_TOTP_URL_5: u64 = 43;
    const FID_TOTP_URL_6: u64 = 44;

    const FID_TOTP_SECRET_0: u64 = 50;
    const FID_TOTP_SECRET_1: u64 = 51;

    const FID_TOTP_NOW: u64 = 60;
    const FID_VERSION: u64 = 61;

    struct Ext;

    // ─── arg coercion helpers ───

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_u32(args: &[SqlValue], i: usize, fname: &str) -> Result<u32, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) if *n >= 0 && *n <= u32::MAX as i64 => Ok(*n as u32),
            _ => Err(format!("{fname}: non-negative INTEGER arg at {i}")),
        }
    }

    fn arg_u64(args: &[SqlValue], i: usize, fname: &str) -> Result<u64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) if *n >= 0 => Ok(*n as u64),
            _ => Err(format!("{fname}: non-negative INTEGER arg at {i}")),
        }
    }

    /// Decode a base32 secret arg. Errors here flow up to the
    /// SQL layer as a runtime error  callers picking the wrong
    /// column hit it loudly. The verify paths short-circuit to
    /// 0 instead, by checking decode upfront.
    fn arg_secret(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        let s = arg_text(args, i, fname)?;
        super::b32_decode(&s).map_err(|e| format!("{fname}: {e}"))
    }

    /// Current epoch seconds. std::time::SystemTime on wasm32-wasip2
    /// reads wasi:clocks/wall-clock under the preview1 adapter, which
    /// the host binds to the real wall clock. Same path the `ids`
    /// extension uses for ms-precision time.
    fn now_secs() -> Result<u64, String> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .map_err(|e| format!("clock before unix epoch: {e}"))
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // TOTP-generate is *not* deterministic in the SQL sense
            // because the result depends on wall-clock time. The
            // generate-at-counter / verify paths are pure given
            // their inputs but TOTP_GENERATE / TOTP_NOW are NOT,
            // so we mark conservatively  marking non-det disables
            // SQLite's function-call caching and prevents indices
            // from using a generated column over totp_generate().
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: f,
            };
            Manifest {
                name: "totp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // totp_generate(secret, [period], [digits], [alg])
                    s(FID_TOTP_GENERATE_1, "totp_generate", 1, nd),
                    s(FID_TOTP_GENERATE_2, "totp_generate", 2, nd),
                    s(FID_TOTP_GENERATE_3, "totp_generate", 3, nd),
                    s(FID_TOTP_GENERATE_4, "totp_generate", 4, nd),
                    // totp_verify(code, secret, [period], [digits], [alg], [window])
                    s(FID_TOTP_VERIFY_2, "totp_verify", 2, nd),
                    s(FID_TOTP_VERIFY_3, "totp_verify", 3, nd),
                    s(FID_TOTP_VERIFY_4, "totp_verify", 4, nd),
                    s(FID_TOTP_VERIFY_5, "totp_verify", 5, nd),
                    s(FID_TOTP_VERIFY_6, "totp_verify", 6, nd),
                    // hotp_generate(secret, counter, [digits], [alg])
                    //  HOTP is deterministic given (secret, counter).
                    s(FID_HOTP_GENERATE_2, "hotp_generate", 2, det),
                    s(FID_HOTP_GENERATE_3, "hotp_generate", 3, det),
                    s(FID_HOTP_GENERATE_4, "hotp_generate", 4, det),
                    // hotp_verify(code, secret, counter, [digits], [alg])
                    s(FID_HOTP_VERIFY_3, "hotp_verify", 3, det),
                    s(FID_HOTP_VERIFY_4, "hotp_verify", 4, det),
                    s(FID_HOTP_VERIFY_5, "hotp_verify", 5, det),
                    // totp_url(label, secret, [issuer], [period], [digits], [alg])
                    s(FID_TOTP_URL_2, "totp_url", 2, det),
                    s(FID_TOTP_URL_3, "totp_url", 3, det),
                    s(FID_TOTP_URL_4, "totp_url", 4, det),
                    s(FID_TOTP_URL_5, "totp_url", 5, det),
                    s(FID_TOTP_URL_6, "totp_url", 6, det),
                    // totp_secret([byte_len])  uses getrandom => non-det.
                    s(FID_TOTP_SECRET_0, "totp_secret", 0, nd),
                    s(FID_TOTP_SECRET_1, "totp_secret", 1, nd),
                    s(FID_TOTP_NOW, "totp_now", 0, nd),
                    s(FID_VERSION, "totp_version", 0, det),
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

    /// Resolve optional (period, digits, alg) trailing args with
    /// the RFC 6238 defaults. `n` is how many of the three are
    /// present in `args` (in order: period, digits, alg).
    fn resolve_totp_opts(
        args: &[SqlValue],
        base: usize,
        n: usize,
        fname: &str,
    ) -> Result<(u32, u32, super::Alg), String> {
        let period = if n >= 1 {
            let p = arg_u32(args, base, fname)?;
            if p == 0 {
                return Err(format!("{fname}: period_s must be > 0"));
            }
            p
        } else {
            30
        };
        let digits = if n >= 2 {
            arg_u32(args, base + 1, fname)?
        } else {
            6
        };
        let alg = if n >= 3 {
            let a = arg_text(args, base + 2, fname)?;
            super::Alg::parse(&a).map_err(|e| format!("{fname}: {e}"))?
        } else {
            super::Alg::Sha1
        };
        Ok((period, digits, alg))
    }

    fn resolve_hotp_opts(
        args: &[SqlValue],
        base: usize,
        n: usize,
        fname: &str,
    ) -> Result<(u32, super::Alg), String> {
        let digits = if n >= 1 {
            arg_u32(args, base, fname)?
        } else {
            6
        };
        let alg = if n >= 2 {
            let a = arg_text(args, base + 1, fname)?;
            super::Alg::parse(&a).map_err(|e| format!("{fname}: {e}"))?
        } else {
            super::Alg::Sha1
        };
        Ok((digits, alg))
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                // ─── totp_generate ───
                FID_TOTP_GENERATE_1
                | FID_TOTP_GENERATE_2
                | FID_TOTP_GENERATE_3
                | FID_TOTP_GENERATE_4 => {
                    let fname = "totp_generate";
                    let n_opts = (func_id - FID_TOTP_GENERATE_1) as usize;
                    let secret = arg_secret(&args, 0, fname)?;
                    let (period, digits, alg) =
                        resolve_totp_opts(&args, 1, n_opts, fname)?;
                    let now = now_secs()?;
                    super::totp_at(&secret, now, period, digits, alg).map(SqlValue::Text)
                }
                // ─── totp_verify ───
                FID_TOTP_VERIFY_2
                | FID_TOTP_VERIFY_3
                | FID_TOTP_VERIFY_4
                | FID_TOTP_VERIFY_5
                | FID_TOTP_VERIFY_6 => {
                    let fname = "totp_verify";
                    let n_after = (func_id - FID_TOTP_VERIFY_2) as usize;
                    let code = arg_text(&args, 0, fname)?;
                    // Decode failure in verify => 0, not error.
                    let Ok(secret_s) = arg_text(&args, 1, fname) else {
                        return Ok(SqlValue::Integer(0));
                    };
                    let Ok(secret) = super::b32_decode(&secret_s) else {
                        return Ok(SqlValue::Integer(0));
                    };
                    // n_after = optional args present after (code, secret).
                    // Layout: [period?, digits?, alg?, window?]. We resolve
                    // the first up-to-3 via the totp helper, then the
                    // window separately.
                    let totp_n = n_after.min(3);
                    let (period, digits, alg) = match resolve_totp_opts(
                        &args, 2, totp_n, fname,
                    ) {
                        Ok(v) => v,
                        Err(_) => return Ok(SqlValue::Integer(0)),
                    };
                    let window = if n_after >= 4 {
                        match arg_u32(&args, 5, fname) {
                            Ok(w) => w,
                            Err(_) => return Ok(SqlValue::Integer(0)),
                        }
                    } else {
                        1
                    };
                    let now = match now_secs() {
                        Ok(n) => n,
                        Err(_) => return Ok(SqlValue::Integer(0)),
                    };
                    let ok = super::totp_verify_at(
                        &code, &secret, now, period, digits, alg, window,
                    );
                    Ok(SqlValue::Integer(ok as i64))
                }
                // ─── hotp_generate ───
                FID_HOTP_GENERATE_2
                | FID_HOTP_GENERATE_3
                | FID_HOTP_GENERATE_4 => {
                    let fname = "hotp_generate";
                    let n_opts = (func_id - FID_HOTP_GENERATE_2) as usize;
                    let secret = arg_secret(&args, 0, fname)?;
                    let counter = arg_u64(&args, 1, fname)?;
                    let (digits, alg) = resolve_hotp_opts(&args, 2, n_opts, fname)?;
                    super::hotp(&secret, counter, digits, alg).map(SqlValue::Text)
                }
                // ─── hotp_verify ───
                FID_HOTP_VERIFY_3 | FID_HOTP_VERIFY_4 | FID_HOTP_VERIFY_5 => {
                    let fname = "hotp_verify";
                    let n_opts = (func_id - FID_HOTP_VERIFY_3) as usize;
                    let code = arg_text(&args, 0, fname)?;
                    let Ok(secret_s) = arg_text(&args, 1, fname) else {
                        return Ok(SqlValue::Integer(0));
                    };
                    let Ok(secret) = super::b32_decode(&secret_s) else {
                        return Ok(SqlValue::Integer(0));
                    };
                    let counter = match arg_u64(&args, 2, fname) {
                        Ok(c) => c,
                        Err(_) => return Ok(SqlValue::Integer(0)),
                    };
                    let (digits, alg) = match resolve_hotp_opts(&args, 3, n_opts, fname)
                    {
                        Ok(v) => v,
                        Err(_) => return Ok(SqlValue::Integer(0)),
                    };
                    let ok = super::hotp_verify(&code, &secret, counter, digits, alg);
                    Ok(SqlValue::Integer(ok as i64))
                }
                // ─── totp_url ───
                FID_TOTP_URL_2
                | FID_TOTP_URL_3
                | FID_TOTP_URL_4
                | FID_TOTP_URL_5
                | FID_TOTP_URL_6 => {
                    let fname = "totp_url";
                    let n_opts = (func_id - FID_TOTP_URL_2) as usize;
                    let label = arg_text(&args, 0, fname)?;
                    let secret = arg_secret(&args, 1, fname)?;
                    // Layout after (label, secret): [issuer?, period?, digits?, alg?]
                    let issuer = if n_opts >= 1 {
                        Some(arg_text(&args, 2, fname)?)
                    } else {
                        None
                    };
                    let period = if n_opts >= 2 {
                        let p = arg_u32(&args, 3, fname)?;
                        if p == 0 {
                            return Err(format!("{fname}: period_s must be > 0"));
                        }
                        p
                    } else {
                        30
                    };
                    let digits = if n_opts >= 3 {
                        arg_u32(&args, 4, fname)?
                    } else {
                        6
                    };
                    let alg = if n_opts >= 4 {
                        let a = arg_text(&args, 5, fname)?;
                        super::Alg::parse(&a).map_err(|e| format!("{fname}: {e}"))?
                    } else {
                        super::Alg::Sha1
                    };
                    Ok(SqlValue::Text(super::totp_url(
                        &label,
                        &secret,
                        issuer.as_deref(),
                        period,
                        digits,
                        alg,
                    )))
                }
                // ─── totp_secret ───
                FID_TOTP_SECRET_0 => super::random_secret(20).map(SqlValue::Text),
                FID_TOTP_SECRET_1 => {
                    let n = arg_u32(&args, 0, "totp_secret")?;
                    super::random_secret(n).map(SqlValue::Text)
                }
                // ─── totp_now ───
                FID_TOTP_NOW => Ok(SqlValue::Integer(now_secs()? as i64)),
                // ─── version ───
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("totp: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
