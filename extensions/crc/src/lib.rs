//! CRC-32 / CRC-64 / CRC-16 checksums (multiple polynomials)

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

// wasm_export is gated off in embed builds  the WIT export
// symbols would collide with any other embedded extension's.
// See PLAN-embed-extensions.md.
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

    use crc::{Crc, CRC_16_ARC, CRC_32_BZIP2, CRC_32_ISO_HDLC, CRC_64_ECMA_182, CRC_64_XZ};

    const FID_CRC32: u64 = 1;
    const FID_CRC32_BZIP2: u64 = 2;
    const FID_CRC64: u64 = 3;
    const FID_CRC64_XZ: u64 = 4;
    const FID_CRC16: u64 = 5;

    struct Ext;

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
                name: "crc".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_CRC32, "crc32", 1, det),
                    s(FID_CRC32_BZIP2, "crc32_bzip2", 1, det),
                    s(FID_CRC64, "crc64_ecma", 1, det),
                    s(FID_CRC64_XZ, "crc64_xz", 1, det),
                    s(FID_CRC16, "crc16", 1, det),
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
            let bytes = arg_blob(&args, 0, "crc")?;

            match func_id {
                FID_CRC32 => {
                    let crc = Crc::<u32>::new(&CRC_32_ISO_HDLC);
                    Ok(SqlValue::Integer(crc.checksum(&bytes) as i64))
                }
                FID_CRC32_BZIP2 => {
                    let crc = Crc::<u32>::new(&CRC_32_BZIP2);
                    Ok(SqlValue::Integer(crc.checksum(&bytes) as i64))
                }
                FID_CRC64 => {
                    let crc = Crc::<u64>::new(&CRC_64_ECMA_182);
                    Ok(SqlValue::Integer(crc.checksum(&bytes) as i64))
                }
                FID_CRC64_XZ => {
                    let crc = Crc::<u64>::new(&CRC_64_XZ);
                    Ok(SqlValue::Integer(crc.checksum(&bytes) as i64))
                }
                FID_CRC16 => {
                    let crc = Crc::<u16>::new(&CRC_16_ARC);
                    Ok(SqlValue::Integer(crc.checksum(&bytes) as i64))
                }
                other => Err(format!("crc: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
