//! OpenPGP key parsing + signature verification for SQL (RFC 4880).
//!
//! Function surface (parse-only + verify; signing/keygen deferred):
//!   pgp_key_id(blob_or_armored)         -> text (lowercase hex)
//!   pgp_key_fingerprint(blob_or_armored) -> text (lowercase hex)
//!   pgp_key_user_ids(blob_or_armored)   -> text (JSON array of UIDs)
//!   pgp_key_algorithm(blob_or_armored)  -> text
//!   pgp_key_bits(blob_or_armored)       -> integer
//!   pgp_key_created(blob_or_armored)    -> text (ISO 8601 UTC)
//!   pgp_armor_to_binary(armored_text)   -> blob
//!   pgp_binary_to_armor(blob, type)     -> text
//!   pgp_verify(sig, message, pubkey)    -> integer (0/1)
//!   pgp_version()                       -> text
//!
//! Input coercion: every public-key input accepts TEXT (treated as
//! armored ASCII) or BLOB (auto-detected: starts with "-----BEGIN"
//! => armored, otherwise raw OpenPGP binary).

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

    use pgp::armor::BlockType;
    use pgp::composed::{Deserializable, SignedPublicKey, StandaloneSignature};
    use pgp::crypto::public_key::PublicKeyAlgorithm;
    use pgp::ser::Serialize as PgpSerialize;
    use pgp::types::{KeyTrait, PublicParams};

    // ---- Function IDs ----------------------------------------------------
    const FID_KEY_ID: u64 = 1;
    const FID_FINGERPRINT: u64 = 2;
    const FID_USER_IDS: u64 = 3;
    const FID_ALGORITHM: u64 = 4;
    const FID_BITS: u64 = 5;
    const FID_CREATED: u64 = 6;
    const FID_ARMOR_TO_BIN: u64 = 7;
    const FID_BIN_TO_ARMOR: u64 = 8;
    const FID_VERIFY: u64 = 9;
    const FID_VERSION: u64 = 10;

    struct Ext;

    // ---- Input coercion --------------------------------------------------

    /// Coerce a SQL value into raw OpenPGP binary bytes. Accepts:
    ///   * TEXT  treat as ASCII-armored input, dearmor first
    ///   * BLOB  auto-detect ("-----BEGIN" prefix => armored, else
    ///           assume already binary)
    ///   * NULL / INT / REAL  None
    fn coerce_binary(v: &SqlValue) -> Option<Vec<u8>> {
        match v {
            SqlValue::Text(s) => dearmor(s.as_bytes()),
            SqlValue::Blob(b) => {
                if b.starts_with(b"-----BEGIN") {
                    dearmor(b)
                } else {
                    Some(b.clone())
                }
            }
            _ => None,
        }
    }

    /// Read ASCII-armored OpenPGP data and return the de-armored
    /// binary payload (header + base64 body parsed, checksum
    /// ignored on failure). Returns None on parse failure.
    fn dearmor(bytes: &[u8]) -> Option<Vec<u8>> {
        use std::io::Read;
        let cursor = std::io::Cursor::new(bytes);
        let mut d = pgp::armor::Dearmor::new(std::io::BufReader::new(cursor));
        if d.read_header().is_err() {
            return None;
        }
        let mut out = Vec::new();
        // Read until EOF or a body error. The dearmor's Read impl
        // returns the decoded base64 bytes (the OpenPGP packet
        // stream).
        match d.read_to_end(&mut out) {
            Ok(_) => Some(out),
            // Even on truncation we may have a usable prefix; but
            // for safety treat any error as a failure.
            Err(_) => None,
        }
    }

    fn coerce_key(v: &SqlValue) -> Option<SignedPublicKey> {
        let bin = coerce_binary(v)?;
        // rPGP's PacketParser panics on certain malformed inputs
        // (slice-index out-of-bounds when the encoded packet length
        // exceeds the actual input length). wasm32-wasip2 uses
        // panic=abort, so catch_unwind is a no-op  we pre-validate
        // packet headers instead.
        if !packet_stream_sane(&bin) {
            return None;
        }
        SignedPublicKey::from_bytes(std::io::Cursor::new(bin)).ok()
    }

    /// Sanity-check an OpenPGP packet stream: every packet header
    /// must have bit 7 set (RFC 4880 4.2), and the declared length
    /// must not exceed the remaining input. Returns false on the
    /// first malformed header.
    ///
    /// This is defensive  rPGP's PacketParser otherwise panics
    /// (slice-index out-of-bounds) on hand-crafted nonsense.
    fn packet_stream_sane(buf: &[u8]) -> bool {
        if buf.is_empty() {
            return false;
        }
        let mut i = 0;
        while i < buf.len() {
            let tag = buf[i];
            if tag & 0x80 == 0 {
                return false; // not a packet header
            }
            i += 1;
            let (body_len, consumed) = if tag & 0x40 == 0 {
                // Old format: low 2 bits of tag describe length type.
                match tag & 0x3 {
                    0 => {
                        if i + 1 > buf.len() { return false; }
                        let l = buf[i] as usize;
                        (Some(l), 1)
                    }
                    1 => {
                        if i + 2 > buf.len() { return false; }
                        let l = u16::from_be_bytes([buf[i], buf[i+1]]) as usize;
                        (Some(l), 2)
                    }
                    2 => {
                        if i + 4 > buf.len() { return false; }
                        let l = u32::from_be_bytes([buf[i], buf[i+1], buf[i+2], buf[i+3]]) as usize;
                        (Some(l), 4)
                    }
                    _ => (None, 0), // indeterminate  read to end
                }
            } else {
                // New format.
                if i >= buf.len() { return false; }
                let o = buf[i];
                if o < 192 {
                    (Some(o as usize), 1)
                } else if o < 224 {
                    if i + 2 > buf.len() { return false; }
                    let l = (((o as usize) - 192) << 8) + buf[i+1] as usize + 192;
                    (Some(l), 2)
                } else if o < 255 {
                    // Partial length  treat as variable, just step.
                    (None, 1)
                } else {
                    if i + 5 > buf.len() { return false; }
                    let l = u32::from_be_bytes([buf[i+1], buf[i+2], buf[i+3], buf[i+4]]) as usize;
                    (Some(l), 5)
                }
            };
            i += consumed;
            match body_len {
                Some(l) => {
                    // Reject implausibly large packets (>16MiB)
                    // and lengths that overrun the buffer.
                    if l > 16 * 1024 * 1024 || i + l > buf.len() {
                        return false;
                    }
                    i += l;
                }
                None => {
                    // Indeterminate / partial  cannot statically
                    // verify the rest; trust the rPGP parser from
                    // here. The header itself was valid.
                    return true;
                }
            }
        }
        true
    }

    // ---- Field extractors ------------------------------------------------

    fn algorithm_name(a: PublicKeyAlgorithm) -> String {
        match a {
            PublicKeyAlgorithm::RSA => "RSA".into(),
            PublicKeyAlgorithm::RSAEncrypt => "RSA-Encrypt".into(),
            PublicKeyAlgorithm::RSASign => "RSA-Sign".into(),
            PublicKeyAlgorithm::ElgamalSign => "Elgamal-Sign".into(),
            PublicKeyAlgorithm::DSA => "DSA".into(),
            PublicKeyAlgorithm::ECDH => "ECDH".into(),
            PublicKeyAlgorithm::ECDSA => "ECDSA".into(),
            PublicKeyAlgorithm::Elgamal => "Elgamal".into(),
            PublicKeyAlgorithm::DiffieHellman => "DH".into(),
            PublicKeyAlgorithm::EdDSA => "EdDSA".into(),
            PublicKeyAlgorithm::Private100
            | PublicKeyAlgorithm::Private101
            | PublicKeyAlgorithm::Private102
            | PublicKeyAlgorithm::Private103
            | PublicKeyAlgorithm::Private104
            | PublicKeyAlgorithm::Private105
            | PublicKeyAlgorithm::Private106
            | PublicKeyAlgorithm::Private107
            | PublicKeyAlgorithm::Private108
            | PublicKeyAlgorithm::Private109
            | PublicKeyAlgorithm::Private110 => "Private".into(),
            PublicKeyAlgorithm::Unknown(n) => format!("Unknown({n})"),
        }
    }

    /// Best-effort bit-length of the primary public key. RSA  modulus
    /// bits; DSA  p prime bits; ECDSA/ECDH/EdDSA  fixed curve sizes;
    /// Elgamal  p prime bits.
    fn key_bits(k: &SignedPublicKey) -> Option<i64> {
        let params = k.primary_key.public_params();
        match params {
            PublicParams::RSA { n, .. } => Some(mpi_bit_len(n.as_bytes())),
            PublicParams::DSA { p, .. } => Some(mpi_bit_len(p.as_bytes())),
            PublicParams::Elgamal { p, .. } => Some(mpi_bit_len(p.as_bytes())),
            PublicParams::ECDSA(ec) => Some(match ec {
                pgp::types::EcdsaPublicParams::P256 { .. } => 256,
                pgp::types::EcdsaPublicParams::P384 { .. } => 384,
                pgp::types::EcdsaPublicParams::P521 { .. } => 521,
                pgp::types::EcdsaPublicParams::Secp256k1 { .. } => 256,
                pgp::types::EcdsaPublicParams::Unsupported { p, .. } => mpi_bit_len(p.as_bytes()),
            }),
            PublicParams::ECDH { curve, p, .. } => {
                Some(curve_bits(curve).unwrap_or_else(|| mpi_bit_len(p.as_bytes())))
            }
            PublicParams::EdDSA { curve, q } => {
                Some(curve_bits(curve).unwrap_or_else(|| mpi_bit_len(q.as_bytes())))
            }
            PublicParams::Unknown { .. } => None,
        }
    }

    fn curve_bits(curve: &pgp::crypto::ecc_curve::ECCCurve) -> Option<i64> {
        use pgp::crypto::ecc_curve::ECCCurve;
        Some(match curve {
            ECCCurve::P256 => 256,
            ECCCurve::P384 => 384,
            ECCCurve::P521 => 521,
            ECCCurve::Secp256k1 => 256,
            ECCCurve::Ed25519 => 256,
            ECCCurve::Curve25519 => 256,
            ECCCurve::BrainpoolP256r1 => 256,
            ECCCurve::BrainpoolP384r1 => 384,
            ECCCurve::BrainpoolP512r1 => 512,
            _ => return None,
        })
    }

    /// Bit length of an Mpi (big-endian, no leading sign byte).
    fn mpi_bit_len(bytes: &[u8]) -> i64 {
        let mut i = 0;
        while i < bytes.len() && bytes[i] == 0 {
            i += 1;
        }
        if i == bytes.len() {
            return 0;
        }
        let leading = bytes[i].leading_zeros() as usize;
        ((bytes.len() - i) * 8 - leading) as i64
    }

    fn user_ids_json(k: &SignedPublicKey) -> String {
        let mut arr: Vec<serde_json::Value> = Vec::new();
        for u in &k.details.users {
            // UID payload is bytes; OpenPGP UIDs are usually UTF-8
            // (RFC 4880 5.11). Lossy decode so a malformed UID
            // still renders.
            let s = String::from_utf8_lossy(u.id.id().as_ref()).into_owned();
            arr.push(serde_json::Value::String(s));
        }
        serde_json::Value::Array(arr).to_string()
    }

    fn fmt_created(k: &SignedPublicKey) -> String {
        let dt = k.primary_key.created_at();
        // chrono::DateTime<Utc> => format with trailing Z, no
        // sub-second component (OpenPGP timestamps are u32 epoch
        // seconds).
        dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }

    // ---- per-fn implementations ------------------------------------------

    fn arg(args: &[SqlValue], i: usize) -> &SqlValue {
        args.get(i).unwrap_or(&SqlValue::Null)
    }

    fn impl_key_id(args: &[SqlValue]) -> SqlValue {
        let Some(k) = coerce_key(arg(args, 0)) else { return SqlValue::Null };
        SqlValue::Text(hex::encode(k.key_id().as_ref()))
    }
    fn impl_fingerprint(args: &[SqlValue]) -> SqlValue {
        let Some(k) = coerce_key(arg(args, 0)) else { return SqlValue::Null };
        SqlValue::Text(hex::encode(k.fingerprint()))
    }
    fn impl_user_ids(args: &[SqlValue]) -> SqlValue {
        let Some(k) = coerce_key(arg(args, 0)) else { return SqlValue::Null };
        SqlValue::Text(user_ids_json(&k))
    }
    fn impl_algorithm(args: &[SqlValue]) -> SqlValue {
        let Some(k) = coerce_key(arg(args, 0)) else { return SqlValue::Null };
        SqlValue::Text(algorithm_name(k.algorithm()))
    }
    fn impl_bits(args: &[SqlValue]) -> SqlValue {
        let Some(k) = coerce_key(arg(args, 0)) else { return SqlValue::Null };
        match key_bits(&k) {
            Some(b) => SqlValue::Integer(b),
            None => SqlValue::Null,
        }
    }
    fn impl_created(args: &[SqlValue]) -> SqlValue {
        let Some(k) = coerce_key(arg(args, 0)) else { return SqlValue::Null };
        SqlValue::Text(fmt_created(&k))
    }

    fn impl_armor_to_binary(args: &[SqlValue]) -> SqlValue {
        let input = match arg(args, 0) {
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Blob(b) => b.clone(),
            _ => return SqlValue::Null,
        };
        match dearmor(&input) {
            Some(b) => SqlValue::Blob(b),
            None => SqlValue::Null,
        }
    }

    fn impl_binary_to_armor(args: &[SqlValue]) -> SqlValue {
        let bin = match arg(args, 0) {
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            _ => return SqlValue::Null,
        };
        let ty = match arg(args, 1) {
            SqlValue::Text(s) => s.to_ascii_lowercase(),
            _ => return SqlValue::Null,
        };
        let block = match ty.as_str() {
            "public" | "public_key" | "publickey" => BlockType::PublicKey,
            "private" | "private_key" | "privatekey" | "secret" => BlockType::PrivateKey,
            "signature" | "sig" => BlockType::Signature,
            "message" | "msg" => BlockType::Message,
            "file" => BlockType::File,
            _ => return SqlValue::Null,
        };
        // Use the raw-bytes serializer the pgp crate exposes for
        // anything Serialize-able. The simplest path: wrap our
        // bytes in a tiny adapter that forwards to_writer.
        struct Raw<'a>(&'a [u8]);
        impl PgpSerialize for Raw<'_> {
            fn to_writer<W: std::io::Write>(&self, w: &mut W) -> pgp::errors::Result<()> {
                w.write_all(self.0)?;
                Ok(())
            }
        }
        let mut out: Vec<u8> = Vec::new();
        if pgp::armor::write(&Raw(&bin), block, &mut out, None, true).is_err() {
            return SqlValue::Null;
        }
        match String::from_utf8(out) {
            Ok(s) => SqlValue::Text(s),
            Err(_) => SqlValue::Null,
        }
    }

    fn impl_verify(args: &[SqlValue]) -> SqlValue {
        // args: sig, message, pubkey
        let Some(sig_bin) = coerce_binary(arg(args, 0)) else { return SqlValue::Integer(0) };
        let msg: Vec<u8> = match arg(args, 1) {
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Blob(b) => b.clone(),
            _ => return SqlValue::Integer(0),
        };
        let Some(key) = coerce_key(arg(args, 2)) else { return SqlValue::Integer(0) };
        // Pre-validate the signature packet stream to avoid the
        // same slice-OOB panic the key path guards against.
        if !packet_stream_sane(&sig_bin) {
            return SqlValue::Integer(0);
        }
        let sig = match StandaloneSignature::from_bytes(std::io::Cursor::new(sig_bin)) {
            Ok(s) => s,
            Err(_) => return SqlValue::Integer(0),
        };
        // Try the primary key first; if that fails, try each
        // public subkey (signing subkeys are common).
        if sig.verify(&key, &msg).is_ok() {
            return SqlValue::Integer(1);
        }
        for sub in &key.public_subkeys {
            if sig.verify(&sub.key, &msg).is_ok() {
                return SqlValue::Integer(1);
            }
        }
        SqlValue::Integer(0)
    }

    // ---- Guest impls -----------------------------------------------------

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
                name: "pgp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_KEY_ID, "pgp_key_id", 1, det),
                    s(FID_FINGERPRINT, "pgp_key_fingerprint", 1, det),
                    s(FID_USER_IDS, "pgp_key_user_ids", 1, det),
                    s(FID_ALGORITHM, "pgp_key_algorithm", 1, det),
                    s(FID_BITS, "pgp_key_bits", 1, det),
                    s(FID_CREATED, "pgp_key_created", 1, det),
                    s(FID_ARMOR_TO_BIN, "pgp_armor_to_binary", 1, det),
                    s(FID_BIN_TO_ARMOR, "pgp_binary_to_armor", 2, det),
                    s(FID_VERIFY, "pgp_verify", 3, det),
                    s(FID_VERSION, "pgp_version", 0, det),
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
                FID_KEY_ID => impl_key_id(&args),
                FID_FINGERPRINT => impl_fingerprint(&args),
                FID_USER_IDS => impl_user_ids(&args),
                FID_ALGORITHM => impl_algorithm(&args),
                FID_BITS => impl_bits(&args),
                FID_CREATED => impl_created(&args),
                FID_ARMOR_TO_BIN => impl_armor_to_binary(&args),
                FID_BIN_TO_ARMOR => impl_binary_to_armor(&args),
                FID_VERIFY => impl_verify(&args),
                FID_VERSION => SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()),
                other => return Err(format!("pgp: unknown func id {other}")),
            })
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
