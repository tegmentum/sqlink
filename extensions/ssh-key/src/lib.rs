//! OpenSSH key file parsing for SQL.
//!
//! Function surface (PLAN-more-extensions-5.md  3, RFC 4253 +
//! RFC 4716 + the wrapped OpenSSH private-key format):
//!   ssh_key_algorithm(s)            -> text
//!   ssh_key_comment(s)              -> text
//!   ssh_key_fingerprint_sha256(s)   -> text  (`SHA256:<base64-no-pad>`)
//!   ssh_key_fingerprint_md5(s)      -> text  (`MD5:aa:bb:...`)
//!   ssh_key_pub_from_priv(priv)     -> text  (`<algo> <base64> <comment>`)
//!   ssh_key_bits(s)                 -> integer
//!   ssh_key_is_encrypted(s)         -> integer
//!   ssh_key_all(s)                  -> text (JSON object)
//!   ssh_key_version()               -> text
//!
//! Input accepts both public-key text (`ssh-rsa AAAAB3... user@host`)
//! and the wrapped OpenSSH private-key PEM block. Encrypted private
//! keys parse for metadata but `ssh_key_pub_from_priv` returns NULL
//! on them (we don't ask for a passphrase).
//!
//! NULL or unparseable input -> NULL on every fn except
//! `ssh_key_version()`.
//!
//! Fingerprints match `ssh-keygen -lf <file>` byte-for-byte:
//!   SHA-256: `SHA256:<unpadded-base64>`  -- the OpenSSH canonical form
//!   MD5:     `MD5:<colon-separated-hex>` -- the legacy form

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

    use base64::Engine as _;
    use md5::Digest;
    use ssh_key::public::KeyData;
    use ssh_key::{Algorithm, EcdsaCurve, HashAlg, PrivateKey, PublicKey};

    const FID_ALGORITHM: u64 = 1;
    const FID_COMMENT: u64 = 2;
    const FID_FP_SHA256: u64 = 3;
    const FID_FP_MD5: u64 = 4;
    const FID_PUB_FROM_PRIV: u64 = 5;
    const FID_BITS: u64 = 6;
    const FID_IS_ENCRYPTED: u64 = 7;
    const FID_ALL: u64 = 8;
    const FID_VERSION: u64 = 9;

    struct Ext;

    // ---- input coercion --------------------------------------------------

    /// One of:
    ///   * Public key parsed from an OpenSSH single-line form
    ///     (`ssh-rsa AAAA... user@host`)
    ///   * Private key parsed from the wrapped PEM block
    ///     (`-----BEGIN OPENSSH PRIVATE KEY-----...`)
    ///
    /// The private key variant carries enough metadata to answer
    /// every field-extracting fn -- algorithm, comment, fingerprint
    /// of the embedded public key, and the encrypted-flag.
    enum Key {
        Public(PublicKey),
        Private(PrivateKey),
    }

    impl Key {
        fn public_key_data(&self) -> Option<&KeyData> {
            match self {
                Key::Public(p) => Some(p.key_data()),
                Key::Private(p) => {
                    // Even when is_encrypted the public key header
                    // is still readable.
                    Some(p.public_key().key_data())
                }
            }
        }

        fn algorithm(&self) -> Algorithm {
            match self {
                Key::Public(p) => p.algorithm(),
                Key::Private(p) => p.algorithm(),
            }
        }

        fn comment(&self) -> &str {
            match self {
                Key::Public(p) => p.comment(),
                Key::Private(p) => p.comment(),
            }
        }
    }

    /// Accept TEXT (UTF-8) or BLOB (ASCII bytes). NULL / INTEGER /
    /// REAL -> None. The wrapped private key form is detected via
    /// the PEM armor; public keys are the single-line ssh-* form.
    fn coerce(v: &SqlValue) -> Option<Key> {
        let bytes: &[u8] = match v {
            SqlValue::Text(s) => s.as_bytes(),
            SqlValue::Blob(b) => b.as_slice(),
            _ => return None,
        };
        // Trim leading whitespace so the PEM armor sniff is robust
        // when callers paste with leading blank lines.
        let trimmed_start = bytes
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(bytes.len());
        let body = &bytes[trimmed_start..];

        if body.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----") {
            return PrivateKey::from_openssh(body).ok().map(Key::Private);
        }
        let s = core::str::from_utf8(body).ok()?;
        PublicKey::from_openssh(s).ok().map(Key::Public)
    }

    fn arg_key(args: &[SqlValue]) -> Option<Key> {
        args.first().and_then(coerce)
    }

    // ---- field extractors ------------------------------------------------

    /// Bit length following the convention reported by
    /// `ssh-keygen -lf`:
    ///   RSA      -> modulus bit length (count significant bits of `n`)
    ///   Ed25519  -> 256 (fixed)
    ///   ECDSA    -> curve order bits (256 / 384 / 521)
    ///   DSA      -> 1024 (RFC 4253 fixes it)
    ///   Other    -> None (NULL)
    fn key_bits(key: &Key) -> Option<i64> {
        let kd = key.public_key_data()?;
        match kd {
            KeyData::Rsa(rsa) => {
                let bytes = rsa.n.as_positive_bytes()?;
                // Count significant bits: skip leading zero bytes,
                // then leading_zeros of the first non-zero byte.
                let mut i = 0;
                while i < bytes.len() && bytes[i] == 0 {
                    i += 1;
                }
                if i == bytes.len() {
                    return Some(0);
                }
                let leading = bytes[i].leading_zeros() as usize;
                Some(((bytes.len() - i) * 8 - leading) as i64)
            }
            KeyData::Ed25519(_) | KeyData::SkEd25519(_) => Some(256),
            KeyData::Ecdsa(ec) => Some(curve_bits(ec.curve())),
            KeyData::SkEcdsaSha2NistP256(_) => Some(256),
            _ => None,
        }
    }

    fn curve_bits(c: EcdsaCurve) -> i64 {
        match c {
            EcdsaCurve::NistP256 => 256,
            EcdsaCurve::NistP384 => 384,
            EcdsaCurve::NistP521 => 521,
        }
    }

    /// `SHA256:<base64-no-pad>` form -- the OpenSSH canonical and
    /// what `ssh-keygen -lf` prints by default. Uses the
    /// `ssh-key::Fingerprint` Display impl which already produces
    /// exactly this string.
    fn fp_sha256(key: &Key) -> Option<String> {
        let kd = key.public_key_data()?;
        let fp = kd.fingerprint(HashAlg::Sha256);
        Some(fp.to_string())
    }

    /// `MD5:aa:bb:cc:...` form -- the legacy `ssh-keygen -E md5`
    /// output. `ssh-key` 0.6's `Fingerprint` only carries SHA-256
    /// and SHA-512, so we hash the SSH-wire public-key blob with
    /// `md-5` ourselves. `PublicKey::to_bytes` produces exactly the
    /// blob OpenSSH hashes (algorithm + key fields, length-prefixed).
    fn fp_md5(key: &Key) -> Option<String> {
        let kd = key.public_key_data()?;
        let stripped = PublicKey::new(kd.clone(), "");
        let blob = stripped.to_bytes().ok()?;
        let mut h = md5::Md5::new();
        h.update(&blob);
        let digest = h.finalize();
        let mut out = String::with_capacity(4 + 3 * digest.len());
        out.push_str("MD5:");
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (i, b) in digest.iter().enumerate() {
            if i > 0 {
                out.push(':');
            }
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xf) as usize] as char);
        }
        Some(out)
    }

    /// Render the public half of a private key as the one-line
    /// `<algorithm> <base64-blob> <comment>` form. Returns None on
    /// encrypted keys (we don't ask for a passphrase, but on the
    /// 0.6 API the public half is decodable even from the armor;
    /// we still gate to keep "encrypted private keys parse metadata
    /// only" per plan acceptance).
    fn pub_text_from_priv(priv_key: &PrivateKey) -> Option<String> {
        if priv_key.is_encrypted() {
            return None;
        }
        priv_key.public_key().to_openssh().ok()
    }

    // ---- per-fn implementations ----------------------------------------

    fn impl_algorithm(args: &[SqlValue]) -> SqlValue {
        let Some(k) = arg_key(args) else { return SqlValue::Null };
        SqlValue::Text(k.algorithm().as_str().to_string())
    }

    fn impl_comment(args: &[SqlValue]) -> SqlValue {
        let Some(k) = arg_key(args) else { return SqlValue::Null };
        SqlValue::Text(k.comment().to_string())
    }

    fn impl_fp_sha256(args: &[SqlValue]) -> SqlValue {
        let Some(k) = arg_key(args) else { return SqlValue::Null };
        match fp_sha256(&k) {
            Some(s) => SqlValue::Text(s),
            None => SqlValue::Null,
        }
    }

    fn impl_fp_md5(args: &[SqlValue]) -> SqlValue {
        let Some(k) = arg_key(args) else { return SqlValue::Null };
        match fp_md5(&k) {
            Some(s) => SqlValue::Text(s),
            None => SqlValue::Null,
        }
    }

    fn impl_pub_from_priv(args: &[SqlValue]) -> SqlValue {
        let Some(k) = arg_key(args) else { return SqlValue::Null };
        match k {
            Key::Private(p) => match pub_text_from_priv(&p) {
                Some(s) => SqlValue::Text(s),
                None => SqlValue::Null,
            },
            // pub-from-priv on a public-key input is a no-op: hand
            // back the canonical one-line form.
            Key::Public(p) => match p.to_openssh() {
                Ok(s) => SqlValue::Text(s),
                Err(_) => SqlValue::Null,
            },
        }
    }

    fn impl_bits(args: &[SqlValue]) -> SqlValue {
        let Some(k) = arg_key(args) else { return SqlValue::Null };
        match key_bits(&k) {
            Some(b) => SqlValue::Integer(b),
            None => SqlValue::Null,
        }
    }

    fn impl_is_encrypted(args: &[SqlValue]) -> SqlValue {
        let Some(k) = arg_key(args) else { return SqlValue::Null };
        match k {
            Key::Public(_) => SqlValue::Integer(0),
            Key::Private(p) => SqlValue::Integer(if p.is_encrypted() { 1 } else { 0 }),
        }
    }

    fn impl_all(args: &[SqlValue]) -> SqlValue {
        let Some(k) = arg_key(args) else { return SqlValue::Null };
        let mut obj = serde_json::Map::new();
        obj.insert("algorithm".into(), k.algorithm().as_str().into());
        obj.insert("comment".into(), k.comment().into());
        if let Some(b) = key_bits(&k) {
            obj.insert("bits".into(), b.into());
        }
        if let Some(s) = fp_sha256(&k) {
            obj.insert("fingerprint_sha256".into(), s.into());
        }
        if let Some(s) = fp_md5(&k) {
            obj.insert("fingerprint_md5".into(), s.into());
        }
        let (is_priv, is_enc) = match &k {
            Key::Private(p) => (true, p.is_encrypted()),
            Key::Public(_) => (false, false),
        };
        obj.insert("is_private".into(), is_priv.into());
        obj.insert("is_encrypted".into(), is_enc.into());
        if let Some(kd) = k.public_key_data() {
            let stripped = PublicKey::new(kd.clone(), "");
            if let Ok(blob) = stripped.to_bytes() {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&blob);
                obj.insert("public_key_blob_b64".into(), b64.into());
            }
        }
        SqlValue::Text(serde_json::Value::Object(obj).to_string())
    }

    // ---- guest impls ----------------------------------------------------

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
                name: "ssh-key".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ALGORITHM, "ssh_key_algorithm", 1, det),
                    s(FID_COMMENT, "ssh_key_comment", 1, det),
                    s(FID_FP_SHA256, "ssh_key_fingerprint_sha256", 1, det),
                    s(FID_FP_MD5, "ssh_key_fingerprint_md5", 1, det),
                    s(FID_PUB_FROM_PRIV, "ssh_key_pub_from_priv", 1, det),
                    s(FID_BITS, "ssh_key_bits", 1, det),
                    s(FID_IS_ENCRYPTED, "ssh_key_is_encrypted", 1, det),
                    s(FID_ALL, "ssh_key_all", 1, det),
                    s(FID_VERSION, "ssh_key_version", 0, det),
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
            Ok(match func_id {
                FID_ALGORITHM => impl_algorithm(&args),
                FID_COMMENT => impl_comment(&args),
                FID_FP_SHA256 => impl_fp_sha256(&args),
                FID_FP_MD5 => impl_fp_md5(&args),
                FID_PUB_FROM_PRIV => impl_pub_from_priv(&args),
                FID_BITS => impl_bits(&args),
                FID_IS_ENCRYPTED => impl_is_encrypted(&args),
                FID_ALL => impl_all(&args),
                FID_VERSION => SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()),
                other => return Err(format!("ssh-key: unknown func id {other}")),
            })
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
