//! classical ciphers (Caesar, ROT13, Vigenere)

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

    const FID_CAESAR_ENCODE: u64 = 1;
    const FID_CAESAR_DECODE: u64 = 2;
    const FID_ROT13: u64 = 3;
    const FID_VIGENERE_ENCODE: u64 = 4;
    const FID_VIGENERE_DECODE: u64 = 5;
    const FID_ATBASH: u64 = 6;

    struct Ext;

    /// Shift one ASCII letter by `n` positions modulo 26.
    /// Preserves case. Non-letters pass through unchanged.
    fn shift_char(c: char, n: i32) -> char {
        let base = if c.is_ascii_uppercase() {
            b'A'
        } else if c.is_ascii_lowercase() {
            b'a'
        } else {
            return c;
        };
        let offset = ((c as u8) - base) as i32;
        let shifted = ((offset + n).rem_euclid(26)) as u8;
        (base + shifted) as char
    }

    fn caesar(text: &str, shift: i32) -> String {
        text.chars().map(|c| shift_char(c, shift)).collect()
    }

    /// Vigenère: each letter shifted by key letter (A=0..Z=25).
    /// Non-letter input chars pass through; non-letter key chars
    /// are skipped (key position only advances on letter shifts).
    fn vigenere(text: &str, key: &str, decode: bool) -> Option<String> {
        let key_shifts: Vec<i32> = key.chars()
            .filter(|c| c.is_ascii_alphabetic())
            .map(|c| (c.to_ascii_uppercase() as u8 - b'A') as i32)
            .collect();
        if key_shifts.is_empty() {
            return None;
        }
        let mut out = String::with_capacity(text.len());
        let mut ki = 0usize;
        for c in text.chars() {
            if c.is_ascii_alphabetic() {
                let n = key_shifts[ki % key_shifts.len()];
                let n = if decode { -n } else { n };
                out.push(shift_char(c, n));
                ki += 1;
            } else {
                out.push(c);
            }
        }
        Some(out)
    }

    /// Atbash: A<->Z, B<->Y, etc. Self-inverse.
    fn atbash(text: &str) -> String {
        text.chars().map(|c| {
            let base = if c.is_ascii_uppercase() {
                b'A'
            } else if c.is_ascii_lowercase() {
                b'a'
            } else {
                return c;
            };
            let offset = (c as u8) - base;
            (base + (25 - offset)) as char
        }).collect()
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
                name: "classic_cipher".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_CAESAR_ENCODE, "caesar_encode", 2, det),
                    s(FID_CAESAR_DECODE, "caesar_decode", 2, det),
                    s(FID_ROT13, "rot13", 1, det),
                    s(FID_VIGENERE_ENCODE, "vigenere_encode", 2, det),
                    s(FID_VIGENERE_DECODE, "vigenere_decode", 2, det),
                    s(FID_ATBASH, "atbash", 1, det),
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
                FID_CAESAR_ENCODE => {
                    let t = arg_text(&args, 0, "caesar_encode")?;
                    let n = arg_int(&args, 1, "caesar_encode")? as i32;
                    Ok(SqlValue::Text(caesar(&t, n)))
                }
                FID_CAESAR_DECODE => {
                    let t = arg_text(&args, 0, "caesar_decode")?;
                    let n = arg_int(&args, 1, "caesar_decode")? as i32;
                    Ok(SqlValue::Text(caesar(&t, -n)))
                }
                FID_ROT13 => {
                    let t = arg_text(&args, 0, "rot13")?;
                    Ok(SqlValue::Text(caesar(&t, 13)))
                }
                FID_VIGENERE_ENCODE => {
                    let t = arg_text(&args, 0, "vigenere_encode")?;
                    let k = arg_text(&args, 1, "vigenere_encode")?;
                    Ok(vigenere(&t, &k, false)
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null))
                }
                FID_VIGENERE_DECODE => {
                    let t = arg_text(&args, 0, "vigenere_decode")?;
                    let k = arg_text(&args, 1, "vigenere_decode")?;
                    Ok(vigenere(&t, &k, true)
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null))
                }
                FID_ATBASH => {
                    let t = arg_text(&args, 0, "atbash")?;
                    Ok(SqlValue::Text(atbash(&t)))
                }
                other => Err(format!("cipher: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
