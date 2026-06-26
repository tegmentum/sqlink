//! URL-safe short IDs (nanoid).
//!
//! See PLAN-extensions-and-handlers.md #4. Three scalars:
//!
//!   nanoid()                  TEXT  21 chars, URL-safe alphabet
//!   nanoid_n(len)             TEXT  custom length, default alphabet
//!   nanoid_alpha(len, alpha)  TEXT  custom alphabet
//!
//! The URL-safe alphabet is the nanoid default: 64 characters
//! `[A-Za-z0-9_-]`. With 21 chars that's ~126 bits of entropy
//! comparable to a v4 UUID's 122 random bits.
//!
//! For `nanoid_alpha`, we sample from the user alphabet using
//! rejection-sampling on uniform u8 bytes so the distribution is
//! unbiased regardless of alphabet size. (The `nanoid` crate's
//! default behaviour mods a u8, which has a < 1/256 bias for
//! alphabets whose size isn't a power of two; we go one step better.)

extern crate alloc;

use alloc::string::String;

/// 64-character URL-safe alphabet  the nanoid default.
/// Matches the `nanoid::nanoid!(N)` form's built-in alphabet.
const URL_SAFE: &[char] = &[
    '_', '-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F', 'G',
    'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z',
];

/// Default size: 21 chars. Matches the nanoid reference implementation
/// (https://github.com/ai/nanoid)  21 chars of the 64-char URL-safe
/// alphabet is ~126 bits, the spec's recommended default.
pub const DEFAULT_SIZE: usize = 21;

/// Upper bound on requested length. The `nanoid` crate's macro form
/// caps similarly; we apply it to all three entrypoints so a runaway
/// `SELECT nanoid_n(1_000_000_000)` can't OOM the host. 256 chars is
/// already absurd for any "short ID" use case.
const MAX_LEN: usize = 256;

/// Generate a default nanoid: 21 chars, URL-safe alphabet.
pub fn nanoid_default() -> String {
    // The `nanoid!` macro takes a literal size only. Use the
    // function form so the size argument can vary at runtime.
    nanoid::nanoid!(DEFAULT_SIZE, URL_SAFE)
}

/// Generate a nanoid of the requested length, URL-safe alphabet.
/// Length is clamped to 1..=256.
pub fn nanoid_n(len: usize) -> String {
    let len = len.clamp(1, MAX_LEN);
    nanoid::nanoid!(len, URL_SAFE)
}

/// Generate a nanoid of the requested length from `alphabet`.
/// Returns Err if the alphabet is empty.
///
/// Uses rejection sampling on uniform u8 to avoid the modulo bias
/// the default nanoid crate exhibits for non-power-of-two alphabets.
pub fn nanoid_alpha(len: usize, alphabet: &str) -> Result<String, String> {
    let chars: alloc::vec::Vec<char> = alphabet.chars().collect();
    if chars.is_empty() {
        return Err("nanoid_alpha: alphabet must be non-empty".into());
    }
    let len = len.clamp(1, MAX_LEN);
    let n = chars.len();
    // Largest multiple of `n` that fits in u8; bytes in [0, threshold)
    // map uniformly to alphabet indices, bytes >= threshold get retried.
    // When n is a power of two, threshold == 256 (modulo 256 in u8) and
    // no retries happen. When n > 128, threshold may be 0 in u16 math;
    // we use u16 to avoid overflow.
    let threshold: u16 = ((256u16 / n as u16) * n as u16) as u16;
    let mut out = String::with_capacity(len);
    let mut written = 0usize;
    let mut buf = [0u8; 64];
    let mut filled = 0usize;
    while written < len {
        if filled == 0 {
            getrandom::getrandom(&mut buf)
                .map_err(|e| alloc::format!("nanoid_alpha: rng: {e}"))?;
            filled = buf.len();
        }
        let b = buf[buf.len() - filled] as u16;
        filled -= 1;
        if b < threshold {
            out.push(chars[(b as usize) % n]);
            written += 1;
        }
    }
    Ok(out)
}

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

    const FID_NANOID: u64 = 1;
    const FID_NANOID_N: u64 = 2;
    const FID_NANOID_ALPHA: u64 = 3;

    struct Ext;

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(r)) => Ok(*r as i64),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Nondeterministic  each call yields fresh randomness.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "nanoid".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NANOID, "nanoid", 0, nd),
                    s(FID_NANOID_N, "nanoid_n", 1, nd),
                    s(FID_NANOID_ALPHA, "nanoid_alpha", 2, nd),
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
                preferred_prefix: Some("nanoid".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.nanoid".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_NANOID => Ok(SqlValue::Text(super::nanoid_default())),
                FID_NANOID_N => {
                    let n = arg_int(&args, 0, "nanoid_n")?;
                    if n <= 0 {
                        return Err(format!("nanoid_n: len must be > 0 (got {n})"));
                    }
                    Ok(SqlValue::Text(super::nanoid_n(n as usize)))
                }
                FID_NANOID_ALPHA => {
                    let n = arg_int(&args, 0, "nanoid_alpha")?;
                    let alpha = arg_text(&args, 1, "nanoid_alpha")?;
                    if n <= 0 {
                        return Err(format!("nanoid_alpha: len must be > 0 (got {n})"));
                    }
                    super::nanoid_alpha(n as usize, &alpha).map(SqlValue::Text)
                }
                other => Err(format!("nanoid: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_21_chars() {
        let id = nanoid_default();
        assert_eq!(id.chars().count(), 21);
        for c in id.chars() {
            assert!(URL_SAFE.contains(&c), "char {c:?} not in url-safe alphabet");
        }
    }

    #[test]
    fn custom_length() {
        assert_eq!(nanoid_n(8).chars().count(), 8);
        assert_eq!(nanoid_n(32).chars().count(), 32);
    }

    #[test]
    fn custom_alphabet_constrains_chars() {
        let id = nanoid_alpha(64, "abc").unwrap();
        assert_eq!(id.chars().count(), 64);
        for c in id.chars() {
            assert!("abc".contains(c));
        }
    }

    #[test]
    fn empty_alphabet_rejected() {
        assert!(nanoid_alpha(8, "").is_err());
    }

    #[test]
    fn no_collisions_in_10k() {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for _ in 0..10_000 {
            let id = nanoid_default();
            assert!(seen.insert(id), "collision in 10k default nanoids");
        }
    }
}
