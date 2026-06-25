//! BLAKE3 hash extension for SQLite.
//!
//! Pairs with the `sha3` (NIST FIPS 202) and `hashes-fast`
//! (xxhash / murmur3) extensions to give three hash families with
//! different tradeoffs:
//!
//!   * sha3       -- standards-track collision-resistance
//!   * blake3     -- modern, fast, with built-in keyed-hash + KDF
//!   * hashes-fast -- sub-nanosecond non-cryptographic hashes
//!
//! Function surface (PLAN-more-extensions-2.md #2):
//!
//!   blake3_hash(value, [output_len])    -> blob (default 32 bytes)
//!   blake3_hex(value, [output_len])     -> text (lowercase hex)
//!   blake3_keyed(key, value, [out_len]) -> blob   (key MUST be 32 bytes)
//!   blake3_keyed_hex(key, value, [out_len]) -> text
//!   blake3_derive_key(context_str, key_material) -> blob (32 bytes; KDF mode)
//!   blake3_version() -> text
//!
//! Value coercion mirrors hashes-fast / sha3: TEXT -> utf-8 bytes,
//! BLOB as-is, INTEGER/REAL -> their TEXT representation,
//! NULL -> empty. Output length is clamped to 1..=65536 bytes (64
//! KiB) -- BLAKE3 is an XOF so longer outputs are well-defined, but
//! we cap to a sane SQL value.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
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

    const FID_HASH: u64 = 1;
    const FID_HEX: u64 = 2;
    const FID_KEYED: u64 = 3;
    const FID_KEYED_HEX: u64 = 4;
    const FID_DERIVE_KEY: u64 = 5;
    const FID_VERSION: u64 = 6;

    /// Cap output length to 64 KiB. BLAKE3 is an XOF so any length is
    /// well-defined; capping avoids absurd allocations from a typo'd
    /// `out_len` argument.
    const MAX_OUT_LEN: usize = 65536;

    struct Ext;

    /// Coerce SqlValue -> bytes for hashing. Matches sha3 +
    /// hashes-fast: TEXT -> utf-8, BLOB as-is, INTEGER/REAL -> their
    /// TEXT representation, NULL -> empty input.
    fn bytes_of(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Integer(n) => n.to_string().into_bytes(),
            SqlValue::Real(r) => r.to_string().into_bytes(),
            SqlValue::Null => Vec::new(),
        }
    }

    /// Parse the optional output-length argument. NULL or missing
    /// means default-32. Anything other than a positive INTEGER in
    /// range is an error -- a typo'd len shouldn't silently produce
    /// the wrong digest size.
    fn opt_out_len(args: &[SqlValue], idx: usize, fname: &str) -> Result<usize, String> {
        match args.get(idx) {
            None | Some(SqlValue::Null) => Ok(32),
            Some(SqlValue::Integer(n)) => {
                if *n < 1 {
                    return Err(format!("{fname}: output_len must be >= 1"));
                }
                let n = *n as usize;
                if n > MAX_OUT_LEN {
                    return Err(format!(
                        "{fname}: output_len {n} exceeds cap {MAX_OUT_LEN}"
                    ));
                }
                Ok(n)
            }
            Some(_) => Err(format!("{fname}: output_len must be INTEGER")),
        }
    }

    /// Hash with XOF expansion to `out_len`. For out_len == 32 this
    /// is exactly the 32-byte digest.
    fn hash_xof(data: &[u8], out_len: usize) -> Vec<u8> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(data);
        let mut buf = vec![0u8; out_len];
        hasher.finalize_xof().fill(&mut buf);
        buf
    }

    /// Keyed hash with 32-byte key. Errors if the key isn't exactly
    /// 32 bytes -- BLAKE3's keyed-hash spec is rigid here.
    fn keyed_hash(key: &[u8], data: &[u8], out_len: usize) -> Result<Vec<u8>, String> {
        if key.len() != 32 {
            return Err(format!(
                "blake3_keyed: key must be exactly 32 bytes (got {})",
                key.len()
            ));
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(key);
        let mut hasher = blake3::Hasher::new_keyed(&k);
        hasher.update(data);
        let mut buf = vec![0u8; out_len];
        hasher.finalize_xof().fill(&mut buf);
        Ok(buf)
    }

    /// Pull the key (first arg) for the keyed variants. TEXT is
    /// accepted as its utf-8 byte view (handy for ASCII test
    /// vectors); BLOB as-is. NULL / INTEGER / REAL are rejected --
    /// a 32-byte key smells like blob input only.
    fn key_bytes(args: &[SqlValue], fname: &str) -> Result<Vec<u8>, String> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: key must be BLOB or TEXT (32 bytes)")),
        }
    }

    /// Pull the context string for derive_key. BLAKE3's KDF spec
    /// requires this to be a UTF-8 string (NOT bytes) -- it's a
    /// domain-separation label, not key material. Reject non-TEXT
    /// so a caller can't accidentally feed it a binary key blob.
    fn context_str(args: &[SqlValue]) -> Result<String, String> {
        match args.first() {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err("blake3_derive_key: context arg must be TEXT".into()),
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
                name: "blake3".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // num_args = -1 advertises a variadic surface so
                    // the optional output_len arg is callable.
                    s(FID_HASH, "blake3_hash", -1, det),
                    s(FID_HEX, "blake3_hex", -1, det),
                    s(FID_KEYED, "blake3_keyed", -1, det),
                    s(FID_KEYED_HEX, "blake3_keyed_hex", -1, det),
                    s(FID_DERIVE_KEY, "blake3_derive_key", 2, det),
                    s(FID_VERSION, "blake3_version", 0, det),
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
            match func_id {
                FID_HASH => {
                    let data = match args.first() {
                        Some(v) => bytes_of(v),
                        None => return Err("blake3_hash: missing data arg".into()),
                    };
                    let out_len = opt_out_len(&args, 1, "blake3_hash")?;
                    Ok(SqlValue::Blob(hash_xof(&data, out_len)))
                }
                FID_HEX => {
                    let data = match args.first() {
                        Some(v) => bytes_of(v),
                        None => return Err("blake3_hex: missing data arg".into()),
                    };
                    let out_len = opt_out_len(&args, 1, "blake3_hex")?;
                    Ok(SqlValue::Text(hex::encode(hash_xof(&data, out_len))))
                }
                FID_KEYED => {
                    let key = key_bytes(&args, "blake3_keyed")?;
                    let data = match args.get(1) {
                        Some(v) => bytes_of(v),
                        None => return Err("blake3_keyed: missing data arg".into()),
                    };
                    let out_len = opt_out_len(&args, 2, "blake3_keyed")?;
                    Ok(SqlValue::Blob(keyed_hash(&key, &data, out_len)?))
                }
                FID_KEYED_HEX => {
                    let key = key_bytes(&args, "blake3_keyed_hex")?;
                    let data = match args.get(1) {
                        Some(v) => bytes_of(v),
                        None => return Err("blake3_keyed_hex: missing data arg".into()),
                    };
                    let out_len = opt_out_len(&args, 2, "blake3_keyed_hex")?;
                    Ok(SqlValue::Text(hex::encode(keyed_hash(&key, &data, out_len)?)))
                }
                FID_DERIVE_KEY => {
                    // derive_key takes a context label (TEXT) + key
                    // material (any value, coerced as for hash).
                    // Output is always 32 bytes -- the standard KDF
                    // surface.
                    let ctx = context_str(&args)?;
                    let key_material = match args.get(1) {
                        Some(v) => bytes_of(v),
                        None => return Err("blake3_derive_key: missing key_material arg".into()),
                    };
                    let out = blake3::derive_key(&ctx, &key_material);
                    Ok(SqlValue::Blob(out.to_vec()))
                }
                FID_VERSION => {
                    // Surface the blake3 crate version + our crate's
                    // version. Callers asserting on this can pin the
                    // upstream they expect.
                    let v = format!(
                        "blake3 crate ?; extension {}",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }
                other => Err(format!("blake3: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
