//! XOR cipher  hex codec with repeating key

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

    const FID_ENCODE: u64 = 1;
    const FID_DECODE: u64 = 2;
    const FID_XOR_RAW: u64 = 3;

    struct Ext;

    /// XOR each byte of `data` with the corresponding byte of `key`,
    /// cycling the key. Empty key  None.
    fn xor_bytes(data: &[u8], key: &[u8]) -> Option<Vec<u8>> {
        if key.is_empty() {
            return None;
        }
        Some(data.iter().enumerate()
            .map(|(i, b)| b ^ key[i % key.len()])
            .collect())
    }

    fn hex_encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push_str(&format!("{:02x}", b));
        }
        out
    }

    fn hex_decode(s: &str) -> Option<Vec<u8>> {
        let s = s.trim();
        if s.len() % 2 != 0 {
            return None;
        }
        let mut out = Vec::with_capacity(s.len() / 2);
        let chars: Vec<char> = s.chars().collect();
        for pair in chars.chunks(2) {
            let hi = pair[0].to_digit(16)?;
            let lo = pair[1].to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
        }
        Some(out)
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
                name: "xor_cipher".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE, "xor_encode", 2, det),
                    s(FID_DECODE, "xor_decode", 2, det),
                    s(FID_XOR_RAW, "xor_raw", 2, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_ENCODE => {
                    // text + key  hex(text XOR key-repeating)
                    let text = arg_text(&args, 0, "xor_encode")?;
                    let key = arg_text(&args, 1, "xor_encode")?;
                    Ok(xor_bytes(text.as_bytes(), key.as_bytes())
                        .map(|b| SqlValue::Text(hex_encode(&b)))
                        .unwrap_or(SqlValue::Null))
                }
                FID_DECODE => {
                    // hex + key  text via XOR inverse (same op)
                    let hex_s = arg_text(&args, 0, "xor_decode")?;
                    let key = arg_text(&args, 1, "xor_decode")?;
                    let bytes = match hex_decode(&hex_s) {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(xor_bytes(&bytes, key.as_bytes())
                        .map(|b| match String::from_utf8(b.clone()) {
                            Ok(s) => SqlValue::Text(s),
                            Err(_) => SqlValue::Blob(b),
                        })
                        .unwrap_or(SqlValue::Null))
                }
                FID_XOR_RAW => {
                    // text + key  hex without round-trip-into-text;
                    // useful for binary keys / non-UTF8 input.
                    let text = arg_text(&args, 0, "xor_raw")?;
                    let key = arg_text(&args, 1, "xor_raw")?;
                    Ok(xor_bytes(text.as_bytes(), key.as_bytes())
                        .map(|b| SqlValue::Blob(b))
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("xor: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
