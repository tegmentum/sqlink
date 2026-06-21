//! X.509 v3 certificate parsing for SQL.
//!
//! Function surface  (PLAN-more-extensions-4.md  8, RFC 5280):
//!   cert_subject(pem_or_der)              -> text
//!   cert_issuer(pem_or_der)               -> text
//!   cert_serial(pem_or_der)               -> text (hex)
//!   cert_not_before(pem_or_der)           -> text (ISO 8601 UTC, trailing Z)
//!   cert_not_after(pem_or_der)            -> text (ISO 8601 UTC)
//!   cert_sig_algorithm(pem_or_der)        -> text (e.g. 'sha256WithRSAEncryption')
//!   cert_public_key_algorithm(pem_or_der) -> text (e.g. 'rsaEncryption')
//!   cert_public_key_bits(pem_or_der)      -> integer
//!   cert_sans(pem_or_der)                 -> text (JSON array)
//!   cert_fingerprint_sha256(pem_or_der)   -> text (lowercase hex)
//!   cert_is_valid_now(pem_or_der, [unix_epoch_s]) -> integer
//!   cert_self_signed(pem_or_der)          -> integer
//!   cert_all(pem_or_der)                  -> text (JSON object)
//!   tls_cert_version()                    -> text
//!
//! Input accepts PEM (string with `-----BEGIN CERTIFICATE-----`
//! armor) or raw DER bytes. NULL input or parse failure NULLs
//! the result on every field-extracting fn; tls_cert_version() is
//! the only fn that always returns a non-NULL.
//!
//! Time handling: x509-parser returns `ASN1Time` which is UTC.
//! We render ISO 8601 with a trailing `Z`. `cert_is_valid_now`
//! takes an optional unix-epoch-seconds arg so callers control
//! the "now" (the wasm guest can't read host time portably).

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

    use sha2::Digest;
    // x509-parser's `prelude` re-exports `crate::pem`, which would
    // shadow the standalone `pem` crate (the PEM-envelope decoder
    // we use for the input-coercion path). Import items
    // explicitly to keep both `pem` (the crate) and the x509
    // types in scope.
    use x509_parser::certificate::X509Certificate;
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    use x509_parser::prelude::FromDer;
    use x509_parser::public_key::PublicKey;
    use x509_parser::time::ASN1Time;
    use x509_parser::x509::X509Name;

    const FID_SUBJECT: u64 = 1;
    const FID_ISSUER: u64 = 2;
    const FID_SERIAL: u64 = 3;
    const FID_NOT_BEFORE: u64 = 4;
    const FID_NOT_AFTER: u64 = 5;
    const FID_SIG_ALG: u64 = 6;
    const FID_PK_ALG: u64 = 7;
    const FID_PK_BITS: u64 = 8;
    const FID_SANS: u64 = 9;
    const FID_FP_SHA256: u64 = 10;
    const FID_VALID_NOW: u64 = 11;
    const FID_SELF_SIGNED: u64 = 12;
    const FID_ALL: u64 = 13;
    const FID_VERSION: u64 = 14;

    struct Ext;

    // ---- input coercion ---------------------------------------------------

    /// Coerce the first argument into raw DER bytes. Accepts:
    ///   * TEXT  treat as PEM (must contain a CERTIFICATE block)
    ///   * BLOB  if it begins with the PEM armor try PEM decode,
    ///           otherwise treat as raw DER
    ///   * NULL / INTEGER / REAL  None
    fn coerce_der(v: &SqlValue) -> Option<Vec<u8>> {
        match v {
            SqlValue::Text(s) => decode_pem(s.as_bytes()),
            SqlValue::Blob(b) => {
                // PEM detection: armor starts with "-----BEGIN".
                if b.starts_with(b"-----BEGIN") {
                    decode_pem(b)
                } else {
                    Some(b.clone())
                }
            }
            _ => None,
        }
    }

    fn decode_pem(bytes: &[u8]) -> Option<Vec<u8>> {
        // `pem` 3 accepts multiple PEM blocks; pick the first
        // CERTIFICATE block. Anything else (PRIVATE KEY,
        // PUBLIC KEY, ...) is silently skipped -- callers may
        // hand us a bundle.
        let blocks = pem::parse_many(bytes).ok()?;
        for b in blocks {
            if b.tag() == "CERTIFICATE" {
                return Some(b.into_contents());
            }
        }
        None
    }

    /// Parse DER bytes to an X509Certificate. Returns None on any
    /// nom / DER error -- the caller propagates that to a SQL NULL.
    fn parse_cert(der: &[u8]) -> Option<X509Certificate<'_>> {
        match X509Certificate::from_der(der) {
            Ok((_, cert)) => Some(cert),
            Err(_) => None,
        }
    }

    fn arg_der(args: &[SqlValue]) -> Option<Vec<u8>> {
        args.first().and_then(coerce_der)
    }

    // ---- field extractors -------------------------------------------------

    /// ISO 8601 UTC with trailing `Z`. `ASN1Time` exposes a
    /// `to_datetime` returning `time::OffsetDateTime`. We format
    /// manually to keep the wire shape stable across `time`
    /// versions and to drop sub-second noise -- X.509 times don't
    /// carry sub-second precision per RFC 5280.
    fn fmt_asn1_time(t: ASN1Time) -> String {
        let dt = t.to_datetime();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            dt.year(),
            u8::from(dt.month()),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second()
        )
    }

    /// RDN string in the conventional comma-separated form,
    /// e.g. `CN=example.com, O=Example Inc, C=US`. x509-parser
    /// already gives a `Display` impl on `X509Name` that produces
    /// this; we just stringify.
    fn fmt_name(n: &X509Name) -> String {
        n.to_string()
    }

    fn sig_algorithm_name(cert: &X509Certificate) -> String {
        // signature_algorithm.algorithm is an Oid; the
        // oid-registry lookup gives the canonical name.
        let oid = &cert.signature_algorithm.algorithm;
        oid_short_name(oid)
    }

    fn pk_algorithm_name(cert: &X509Certificate) -> String {
        let oid = &cert.tbs_certificate.subject_pki.algorithm.algorithm;
        oid_short_name(oid)
    }

    /// Map an OID to a human-readable short name. Falls back to
    /// the dotted-decimal string if the registry has no entry.
    fn oid_short_name(oid: &x509_parser::der_parser::oid::Oid) -> String {
        // oid-registry ships with x509-parser; the helper
        // `format_oid` returns the registry name when known.
        if let Some(entry) =
            x509_parser::oid_registry::OidRegistry::default()
                .with_all_crypto()
                .with_x509()
                .with_x962()
                .with_pkcs1()
                .with_pkcs7()
                .with_pkcs9()
                .with_pkcs12()
                .with_nist_algs()
                .with_kdf()
                .get(oid)
        {
            entry.sn().to_string()
        } else {
            oid.to_id_string()
        }
    }

    /// Best-effort bit-length of the certificate's subject public
    /// key. RSA  modulus bits; EC  curve order bits;
    /// Ed25519/Ed448/X25519/X448  the fixed curve sizes.
    fn pk_bits(cert: &X509Certificate) -> Option<i64> {
        let parsed = cert.public_key().parsed().ok()?;
        match parsed {
            PublicKey::RSA(rsa) => {
                // Modulus is big-endian; leading 0x00 sign-byte
                // common. Count significant bits.
                let m = rsa.modulus;
                // skip leading zeros
                let mut i = 0;
                while i < m.len() && m[i] == 0 {
                    i += 1;
                }
                if i == m.len() {
                    return Some(0);
                }
                let leading = m[i].leading_zeros() as usize;
                Some(((m.len() - i) * 8 - leading) as i64)
            }
            PublicKey::EC(ec) => {
                // EC public-key bytes for uncompressed P-256/384/521
                // is 0x04 || X || Y, X+Y each curve-order bytes.
                // bits = (len-1)/2*8.
                let b = ec.data();
                if b.is_empty() {
                    return None;
                }
                Some((((b.len() - 1) / 2) * 8) as i64)
            }
            PublicKey::DSA(b)
            | PublicKey::GostR3410(b)
            | PublicKey::GostR3410_2012(b)
            | PublicKey::Unknown(b) => Some((b.len() * 8) as i64),
        }
    }

    /// JSON array of SubjectAltName values. Each value is a
    /// short-form string like `"DNS:example.com"`,
    /// `"IP:127.0.0.1"`, `"email:ops@example.com"`,
    /// `"URI:https://..."`. Mirrors the shape `openssl x509`
    /// prints -- the SAN extension is a SEQUENCE OF GeneralName,
    /// each variant a different ASN.1 CHOICE.
    fn sans_json(cert: &X509Certificate) -> Option<String> {
        let san_ext = cert.extensions().iter().find_map(|e| match e.parsed_extension() {
            ParsedExtension::SubjectAlternativeName(s) => Some(s),
            _ => None,
        });
        let san = san_ext?;
        let mut arr: Vec<serde_json::Value> = Vec::new();
        for gn in &san.general_names {
            let s = match gn {
                GeneralName::DNSName(n) => format!("DNS:{n}"),
                GeneralName::IPAddress(b) => match b.len() {
                    4 => format!("IP:{}.{}.{}.{}", b[0], b[1], b[2], b[3]),
                    16 => {
                        // Render IPv6 as colon-separated u16s
                        // (no shortening; downstream consumers can
                        // re-canonicalize if they care).
                        let mut parts: Vec<String> = Vec::with_capacity(8);
                        for chunk in b.chunks(2) {
                            parts.push(format!(
                                "{:x}",
                                u16::from_be_bytes([chunk[0], chunk[1]])
                            ));
                        }
                        format!("IP:{}", parts.join(":"))
                    }
                    _ => format!("IP:({})", b.len()),
                },
                GeneralName::RFC822Name(n) => format!("email:{n}"),
                GeneralName::URI(n) => format!("URI:{n}"),
                GeneralName::DirectoryName(n) => format!("DirName:{n}"),
                GeneralName::RegisteredID(o) => format!("RID:{}", o.to_id_string()),
                GeneralName::OtherName(o, _) => format!("otherName:{}", o.to_id_string()),
                GeneralName::X400Address(_) => "x400Address".to_string(),
                GeneralName::EDIPartyName(_) => "ediPartyName".to_string(),
            };
            arr.push(serde_json::Value::String(s));
        }
        Some(serde_json::Value::Array(arr).to_string())
    }

    fn fingerprint_sha256(der: &[u8]) -> String {
        let mut h = sha2::Sha256::new();
        h.update(der);
        hex::encode(h.finalize())
    }

    /// Serial as lowercase hex, no separators. RFC 5280 forbids
    /// negative serials in practice; if x509-parser hands us a
    /// negative one we use its absolute value (matches openssl).
    fn serial_hex(cert: &X509Certificate) -> String {
        // raw_serial is the big-endian DER INTEGER content bytes
        // (may include a leading 0x00 sign byte). Match openssl's
        // `serial=...` output which drops the sign byte.
        let raw = cert.raw_serial();
        let trimmed: &[u8] = if raw.len() > 1 && raw[0] == 0x00 {
            &raw[1..]
        } else {
            raw
        };
        hex::encode(trimmed)
    }

    // ---- per-fn implementations ------------------------------------------

    fn impl_subject(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        SqlValue::Text(fmt_name(c.subject()))
    }
    fn impl_issuer(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        SqlValue::Text(fmt_name(c.issuer()))
    }
    fn impl_serial(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        SqlValue::Text(serial_hex(&c))
    }
    fn impl_not_before(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        SqlValue::Text(fmt_asn1_time(c.validity().not_before))
    }
    fn impl_not_after(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        SqlValue::Text(fmt_asn1_time(c.validity().not_after))
    }
    fn impl_sig_alg(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        SqlValue::Text(sig_algorithm_name(&c))
    }
    fn impl_pk_alg(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        SqlValue::Text(pk_algorithm_name(&c))
    }
    fn impl_pk_bits(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        match pk_bits(&c) {
            Some(b) => SqlValue::Integer(b),
            None => SqlValue::Null,
        }
    }
    fn impl_sans(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        match sans_json(&c) {
            Some(j) => SqlValue::Text(j),
            None => SqlValue::Text("[]".to_string()),
        }
    }
    fn impl_fp_sha256(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        // Validate it parses -- callers shouldn't get a
        // fingerprint of garbage.
        if parse_cert(&d).is_none() {
            return SqlValue::Null;
        }
        SqlValue::Text(fingerprint_sha256(&d))
    }
    fn impl_valid_now(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        // Optional second arg: unix epoch seconds. If absent, fall
        // back to ASN1Time::now() via the cert's helper (the
        // x509-parser std-feature path; on wasip2 it reads from
        // wasi's wall clock).
        let now_secs: Option<i64> = match args.get(1) {
            None | Some(SqlValue::Null) => None,
            Some(SqlValue::Integer(n)) => Some(*n),
            _ => return SqlValue::Null,
        };
        let not_before = c.validity().not_before.timestamp();
        let not_after = c.validity().not_after.timestamp();
        let now = match now_secs {
            Some(n) => n,
            None => match ASN1Time::now() {
                t => t.timestamp(),
            },
        };
        let ok = now >= not_before && now <= not_after;
        SqlValue::Integer(if ok { 1 } else { 0 })
    }
    fn impl_self_signed(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        // Subject == Issuer is the RFC 5280 5.1.2 informal
        // definition. Strict self-signed also verifies the
        // signature against the embedded public key, but the
        // plan's acceptance criterion is the name-equality check.
        let ss = c.subject() == c.issuer();
        SqlValue::Integer(if ss { 1 } else { 0 })
    }
    fn impl_all(args: &[SqlValue]) -> SqlValue {
        let Some(d) = arg_der(args) else { return SqlValue::Null };
        let Some(c) = parse_cert(&d) else { return SqlValue::Null };
        let mut obj = serde_json::Map::new();
        obj.insert("subject".into(), fmt_name(c.subject()).into());
        obj.insert("issuer".into(), fmt_name(c.issuer()).into());
        obj.insert("serial".into(), serial_hex(&c).into());
        obj.insert("not_before".into(), fmt_asn1_time(c.validity().not_before).into());
        obj.insert("not_after".into(), fmt_asn1_time(c.validity().not_after).into());
        obj.insert("sig_algorithm".into(), sig_algorithm_name(&c).into());
        obj.insert("public_key_algorithm".into(), pk_algorithm_name(&c).into());
        if let Some(b) = pk_bits(&c) {
            obj.insert("public_key_bits".into(), b.into());
        }
        let sans = sans_json(&c).unwrap_or_else(|| "[]".to_string());
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&sans) {
            obj.insert("sans".into(), v);
        }
        obj.insert("fingerprint_sha256".into(), fingerprint_sha256(&d).into());
        obj.insert("self_signed".into(), (c.subject() == c.issuer()).into());
        SqlValue::Text(serde_json::Value::Object(obj).to_string())
    }

    // ---- guest impls -----------------------------------------------------

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
                name: "tls-cert".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_SUBJECT, "cert_subject", 1, det),
                    s(FID_ISSUER, "cert_issuer", 1, det),
                    s(FID_SERIAL, "cert_serial", 1, det),
                    s(FID_NOT_BEFORE, "cert_not_before", 1, det),
                    s(FID_NOT_AFTER, "cert_not_after", 1, det),
                    s(FID_SIG_ALG, "cert_sig_algorithm", 1, det),
                    s(FID_PK_ALG, "cert_public_key_algorithm", 1, det),
                    s(FID_PK_BITS, "cert_public_key_bits", 1, det),
                    s(FID_SANS, "cert_sans", 1, det),
                    s(FID_FP_SHA256, "cert_fingerprint_sha256", 1, det),
                    // -1 = variadic so the optional unix-epoch
                    // second arg is callable.
                    s(FID_VALID_NOW, "cert_is_valid_now", -1, det),
                    s(FID_SELF_SIGNED, "cert_self_signed", 1, det),
                    s(FID_ALL, "cert_all", 1, det),
                    s(FID_VERSION, "tls_cert_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Ok(match func_id {
                FID_SUBJECT => impl_subject(&args),
                FID_ISSUER => impl_issuer(&args),
                FID_SERIAL => impl_serial(&args),
                FID_NOT_BEFORE => impl_not_before(&args),
                FID_NOT_AFTER => impl_not_after(&args),
                FID_SIG_ALG => impl_sig_alg(&args),
                FID_PK_ALG => impl_pk_alg(&args),
                FID_PK_BITS => impl_pk_bits(&args),
                FID_SANS => impl_sans(&args),
                FID_FP_SHA256 => impl_fp_sha256(&args),
                FID_VALID_NOW => impl_valid_now(&args),
                FID_SELF_SIGNED => impl_self_signed(&args),
                FID_ALL => impl_all(&args),
                FID_VERSION => SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()),
                other => return Err(format!("tls-cert: unknown func id {other}")),
            })
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
