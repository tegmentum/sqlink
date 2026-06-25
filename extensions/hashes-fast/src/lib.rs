//! Fast non-cryptographic hashes: xxhash family + murmur3.
//!
//! Pairs with the `sha3` extension -- sha3 covers integrity /
//! collision resistance, this covers the sub-nanosecond regime used
//! by bloom filters, sharding, consistent hashing, hash-join keys.
//!
//! Function surface (matches PLAN-extensions-and-handlers #2):
//!
//!   xxh3(value)                  -> integer (low 63 bits of u64)
//!   xxh3_128(value)              -> blob (16 bytes, big-endian)
//!   xxh64(value, [seed])         -> integer
//!   xxh32(value, [seed])         -> integer (32-bit)
//!   murmur3_32(value, [seed])    -> integer
//!   murmur3_128(value, [seed])   -> blob (16 bytes, big-endian)
//!
//! Coercion: TEXT -> utf-8 bytes, BLOB as-is, INTEGER/REAL -> their
//! TEXT representation, NULL hashes as the empty input. Matches the
//! shathree.c / sha3 extension convention so the two extensions are
//! drop-in interchangeable for "hash this value" cases.

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

    const FID_XXH3: u64 = 1;
    const FID_XXH3_128: u64 = 2;
    const FID_XXH64: u64 = 3;
    const FID_XXH32: u64 = 4;
    const FID_MURMUR3_32: u64 = 5;
    const FID_MURMUR3_128: u64 = 6;

    struct Ext;

    /// Treat the SqlValue as bytes for hashing. Matches shathree.c +
    /// the sha3 extension: TEXT -> utf-8, BLOB as-is, INTEGER/REAL
    /// -> their TEXT representation, NULL -> empty.
    fn bytes_of(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Integer(n) => n.to_string().into_bytes(),
            SqlValue::Real(r) => r.to_string().into_bytes(),
            SqlValue::Null => Vec::new(),
        }
    }

    /// Optional second-arg seed; defaults to 0. Accepts INTEGER only.
    /// REAL / TEXT / BLOB seeds are rejected -- a typo'd seed
    /// shouldn't silently hash to a different bucket.
    fn opt_seed(args: &[SqlValue], fname: &str) -> Result<u64, String> {
        match args.get(1) {
            None | Some(SqlValue::Null) => Ok(0),
            Some(SqlValue::Integer(n)) => Ok(*n as u64),
            Some(_) => Err(format!("{fname}: seed must be INTEGER")),
        }
    }

    /// xxh3 / xxh64 return u64 -- but SQL INTEGER is i64. We cast
    /// bit-for-bit (`as i64`) rather than masking off the top bit:
    /// the round-trip is exact, and SQLite users who want the
    /// unsigned view can re-cast.
    fn u64_to_int(v: u64) -> SqlValue {
        SqlValue::Integer(v as i64)
    }

    /// 32-bit hashes fit fully into i64 (high bits zero), so the
    /// returned integer is non-negative.
    fn u32_to_int(v: u32) -> SqlValue {
        SqlValue::Integer(v as i64)
    }

    fn xxh3_128_blob(bytes: &[u8]) -> Vec<u8> {
        // Big-endian: produces the same byte order users see in test
        // vectors / hex dumps from upstream xxhash references.
        xxhash_rust::xxh3::xxh3_128(bytes).to_be_bytes().to_vec()
    }

    fn xxh3_64_int(bytes: &[u8]) -> SqlValue {
        u64_to_int(xxhash_rust::xxh3::xxh3_64(bytes))
    }

    fn xxh64_int(bytes: &[u8], seed: u64) -> SqlValue {
        u64_to_int(xxhash_rust::xxh64::xxh64(bytes, seed))
    }

    fn xxh32_int(bytes: &[u8], seed: u64) -> SqlValue {
        u32_to_int(xxhash_rust::xxh32::xxh32(bytes, seed as u32))
    }

    fn murmur3_32_int(bytes: &[u8], seed: u64) -> Result<SqlValue, String> {
        // murmur3::murmur3_32 reads from a Read impl. Cursor<&[u8]>
        // is allocation-free; the underlying impl never fails for
        // an in-memory slice but the signature is Result so we map.
        let mut cur = std::io::Cursor::new(bytes);
        murmur3::murmur3_32(&mut cur, seed as u32)
            .map(u32_to_int)
            .map_err(|e| format!("murmur3_32: {e}"))
    }

    fn murmur3_128_blob(bytes: &[u8], seed: u64) -> Result<Vec<u8>, String> {
        let mut cur = std::io::Cursor::new(bytes);
        // murmur3_x64_128 is the canonical 64-bit-optimized 128-bit
        // variant; murmur3_x86_128 exists but is a different output
        // function and rarely what callers want. We commit to the
        // x64 flavor; doc'd in this file's header.
        murmur3::murmur3_x64_128(&mut cur, seed as u32)
            .map(|v| v.to_be_bytes().to_vec())
            .map_err(|e| format!("murmur3_128: {e}"))
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
                name: "hashes-fast".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // xxh3 is unseeded by upstream design (uses a
                    // builtin secret); fixed 1 arg.
                    s(FID_XXH3, "xxh3", 1, det),
                    s(FID_XXH3_128, "xxh3_128", 1, det),
                    // Seed is optional -- num_args = -1.
                    s(FID_XXH64, "xxh64", -1, det),
                    s(FID_XXH32, "xxh32", -1, det),
                    s(FID_MURMUR3_32, "murmur3_32", -1, det),
                    s(FID_MURMUR3_128, "murmur3_128", -1, det),
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
                preferred_prefix: Some("hashes_fast".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.hashes_fast".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let data = match args.first() {
                Some(v) => bytes_of(v),
                None => return Err("hashes-fast: missing data arg".into()),
            };
            match func_id {
                FID_XXH3 => Ok(xxh3_64_int(&data)),
                FID_XXH3_128 => Ok(SqlValue::Blob(xxh3_128_blob(&data))),
                FID_XXH64 => {
                    let seed = opt_seed(&args, "xxh64")?;
                    Ok(xxh64_int(&data, seed))
                }
                FID_XXH32 => {
                    let seed = opt_seed(&args, "xxh32")?;
                    Ok(xxh32_int(&data, seed))
                }
                FID_MURMUR3_32 => {
                    let seed = opt_seed(&args, "murmur3_32")?;
                    murmur3_32_int(&data, seed)
                }
                FID_MURMUR3_128 => {
                    let seed = opt_seed(&args, "murmur3_128")?;
                    murmur3_128_blob(&data, seed).map(SqlValue::Blob)
                }
                other => Err(format!("hashes-fast: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
