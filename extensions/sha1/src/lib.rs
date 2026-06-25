//! Standalone SHA-1 hash extension for SQLite.
//!
//! SHA-1 is broken for collision-resistance (SHAttered, 2017) and
//! should not be used for new security designs -- prefer SHA-3 or
//! BLAKE3 from the sibling extensions. We ship it anyway because
//! it's everywhere existing systems already speak SHA-1:
//!
//!   * git object IDs + pack indexes
//!   * Subversion revision hashes
//!   * HMAC-SHA1 in OAuth 1.0 / legacy AWS sig v2
//!   * X.509 certificate fingerprints in legacy PKI
//!   * RFC 6238 TOTP (default HMAC algorithm is SHA-1)
//!
//! Function surface (matches the hashes-fast / sha3 / blake3
//! coercion convention):
//!
//!   sha1_hash(value) -> blob (20 bytes)
//!   sha1_hex(value)  -> text (40-char lowercase hex)
//!
//! Value coercion: TEXT -> utf-8 bytes, BLOB as-is, INTEGER/REAL
//! -> their TEXT representation, NULL -> empty. The canonical
//! empty-input digest is da39a3ee5e6b4b0d3255bfef95601890afd80709.

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

    use sha1::{Digest, Sha1};

    const FID_HASH: u64 = 1;
    const FID_HEX: u64 = 2;

    struct Ext;

    /// Coerce SqlValue -> bytes for hashing. Matches sha3 +
    /// blake3 + hashes-fast: TEXT -> utf-8, BLOB as-is,
    /// INTEGER/REAL -> their TEXT representation, NULL -> empty
    /// input. Empty input is canonical (sha1('') ==
    /// da39a3ee...), so a NULL silently producing that digest
    /// is the documented behavior, not a leak.
    fn bytes_of(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Integer(n) => n.to_string().into_bytes(),
            SqlValue::Real(r) => r.to_string().into_bytes(),
            SqlValue::Null => Vec::new(),
        }
    }

    /// Compute the 20-byte SHA-1 digest of `data`. One-shot --
    /// SHA-1 outputs are always exactly 160 bits, no XOF / no
    /// truncation. We return a Vec<u8> for the blob path; the
    /// hex path just feeds this to hex::encode.
    fn sha1_digest(data: &[u8]) -> Vec<u8> {
        let mut hasher = Sha1::new();
        hasher.update(data);
        hasher.finalize().to_vec()
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
                name: "sha1".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_HASH, "sha1_hash", 1, det),
                    s(FID_HEX, "sha1_hex", 1, det),
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
            let data = match args.first() {
                Some(v) => bytes_of(v),
                None => return Err("sha1: missing data arg".into()),
            };
            match func_id {
                FID_HASH => Ok(SqlValue::Blob(sha1_digest(&data))),
                FID_HEX => Ok(SqlValue::Text(hex::encode(sha1_digest(&data)))),
                other => Err(format!("sha1: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
