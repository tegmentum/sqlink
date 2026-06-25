//! SHA-3 (NIST FIPS 202) port of SQLite shathree.
//!
//! Two consumers, one codebase:
//!
//!   * the `wasm_export` module produces a wasi-p2 component
//!     loadable at runtime via `.load <ext.wasm>`.
//!   * the `embed` module exposes a `register_into(db)` entry
//!     that registers the same scalars directly via
//!     `sqlite3_create_function_v2`. The cli optionally
//!     `dep:sha3-extension` + calls this at startup to embed the
//!     ext at build time, eliminating the WIT boundary cost
//!     measured in PLAN-benchmarks.md (~2.7 us/call).

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Core algorithm  reuseable by both the WIT export and the
/// native-register embed path. Hash `data` with SHA-3 at `bits`
/// output size; raw bytes; None on unsupported bit size. Matches
/// the size selection in SQLite's shathree.c: 224, 256, 384, 512.
pub fn sha3_bytes(data: &[u8], bits: u32) -> Option<Vec<u8>> {
    use sha3::{Digest, Sha3_224, Sha3_256, Sha3_384, Sha3_512};
    match bits {
        224 => Some(Sha3_224::digest(data).to_vec()),
        256 => Some(Sha3_256::digest(data).to_vec()),
        384 => Some(Sha3_384::digest(data).to_vec()),
        512 => Some(Sha3_512::digest(data).to_vec()),
        _ => None,
    }
}

#[cfg(feature = "embed")]
pub mod embed;

// `wasm_export` is the WIT-component build path: defines
// `bindings::export!` which lowers to `sqlite:extension/metadata`
// + `scalar-function` exports. When the cli pulls this in for
// embedding, those exports collide with any other embedded
// extension's (they all advertise the same WIT interface). Gate
// it off in the embed build  the algorithm reaches the cli via
// `pub mod embed` instead.
#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
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

    const FID_SHA3: u64 = 1;
    const FID_SHA3_224: u64 = 2;
    const FID_SHA3_256: u64 = 3;
    const FID_SHA3_384: u64 = 4;
    const FID_SHA3_512: u64 = 5;
    const FID_SHA3_RAW: u64 = 6;

    struct Ext;

    // Reuses crate-level sha3_bytes  same algorithm path the
    // embed registration uses, so an embedded sha3() can't drift
    // from the .load'd sha3().
    use super::sha3_bytes;

    /// Treat the SqlValue as bytes for hashing. Matches shathree.c's
    /// behavior: TEXT becomes its UTF-8 bytes, BLOB as-is, INTEGER
    /// and REAL coerce to their TEXT representation, NULL hashes as
    /// the empty string.
    fn bytes_of(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Integer(n) => n.to_string().into_bytes(),
            SqlValue::Real(r) => r.to_string().into_bytes(),
            SqlValue::Null => Vec::new(),
        }
    }

    // ---- Arg helpers ----
    // The Big Three; copy-pasted into every extension. The
    // scaffold ships them so you delete what you don't need.

    #[allow(dead_code)]
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Available flags  pass `det` for deterministic scalars
            // (most cases), `nd` for ones that produce different
            // output each call (rng / time-of-call / counter).
            #[allow(unused_variables)]
            let det = FunctionFlags::DETERMINISTIC;
            #[allow(unused_variables)]
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "sha3".to_string(),  // matches the SQLite shathree extension naming
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // Matches the SQLite shathree.c surface:
                    //   sha3(X, [N])  N is 224/256/384/512, default 256
                    s(FID_SHA3, "sha3", 2, det),
                    s(FID_SHA3_224, "sha3_224", 1, det),
                    s(FID_SHA3_256, "sha3_256", 1, det),
                    s(FID_SHA3_384, "sha3_384", 1, det),
                    s(FID_SHA3_512, "sha3_512", 1, det),
                    // Extra not in shathree: raw Blob output (skip hex).
                    s(FID_SHA3_RAW, "sha3_raw", 2, det),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            // First arg is the data to hash for every variant.
            let data = match args.first() {
                Some(v) => bytes_of(v),
                None => return Err("sha3: missing data arg".into()),
            };
            let (bits, raw_out) = match func_id {
                FID_SHA3 => {
                    let n = arg_int(&args, 1, "sha3").unwrap_or(256) as u32;
                    (n, false)
                }
                FID_SHA3_224 => (224, false),
                FID_SHA3_256 => (256, false),
                FID_SHA3_384 => (384, false),
                FID_SHA3_512 => (512, false),
                FID_SHA3_RAW => {
                    let n = arg_int(&args, 1, "sha3_raw").unwrap_or(256) as u32;
                    (n, true)
                }
                other => return Err(format!("sha3: unknown func id {other}")),
            };
            match sha3_bytes(&data, bits) {
                Some(bytes) if raw_out => Ok(SqlValue::Blob(bytes)),
                Some(bytes) => Ok(SqlValue::Text(hex::encode(&bytes))),
                None => Ok(SqlValue::Null),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
