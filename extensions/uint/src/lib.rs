//! uint collation  port of SQLite's ext/misc/uint.c.
//!
//! The collation treats strings as natural-numeric sequences: when
//! both operands have a digit run at the same position, the digit
//! runs are compared numerically (longer run wins on tie). Outside
//! digit runs, comparison is byte-wise.
//!
//! Use via:  ORDER BY col COLLATE uint
//!
//! This is the FIRST consumer of the host's collation dispatch path.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "collating",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::collation::Guest as CollationGuest;
    use bindings::exports::sqlite::extension::metadata::{
        CollationSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::SqlValue;

    const CID_UINT: u64 = 1;

    struct Ext;

    /// Port of the comparison logic from ext/misc/uint.c.
    /// Walks both strings in parallel:
    ///   - if both are at a digit  consume the digit run from each,
    ///     compare numerically. If equal numerically, the longer
    ///     digit run wins (e.g. "01" > "1").
    ///   - else  compare byte by byte until a difference or one ends.
    fn uint_compare(a: &str, b: &str) -> i32 {
        let ab = a.as_bytes();
        let bb = b.as_bytes();
        let mut i = 0usize;
        let mut j = 0usize;
        while i < ab.len() && j < bb.len() {
            let ca = ab[i];
            let cb = bb[j];
            let a_digit = ca.is_ascii_digit();
            let b_digit = cb.is_ascii_digit();
            if a_digit && b_digit {
                // Skip leading zeros for numerical-magnitude compare;
                // but remember the original lengths for the tie-break
                // ("01" > "1" because more digits  larger zero-padded).
                let a_start = i;
                let b_start = j;
                while i < ab.len() && ab[i].is_ascii_digit() {
                    i += 1;
                }
                while j < bb.len() && bb[j].is_ascii_digit() {
                    j += 1;
                }
                let a_run = &ab[a_start..i];
                let b_run = &bb[b_start..j];
                let a_nolead = strip_leading_zeros(a_run);
                let b_nolead = strip_leading_zeros(b_run);
                // Compare magnitude (longer = larger).
                let mag = a_nolead.len().cmp(&b_nolead.len());
                if mag != core::cmp::Ordering::Equal {
                    return as_i32(mag);
                }
                // Same magnitude: byte-wise compare of the stripped
                // digit runs.
                let lex = a_nolead.cmp(b_nolead);
                if lex != core::cmp::Ordering::Equal {
                    return as_i32(lex);
                }
                // Magnitudes and digits equal  longer original
                // (more leading zeros) sorts AFTER (matches uint.c).
                let pad = a_run.len().cmp(&b_run.len());
                if pad != core::cmp::Ordering::Equal {
                    return as_i32(pad);
                }
            } else {
                let cmp = ca.cmp(&cb);
                if cmp != core::cmp::Ordering::Equal {
                    return as_i32(cmp);
                }
                i += 1;
                j += 1;
            }
        }
        // Shared prefix matched; shorter wins.
        as_i32(ab.len().cmp(&bb.len()))
    }

    fn strip_leading_zeros(s: &[u8]) -> &[u8] {
        let mut k = 0;
        while k < s.len() - 1 && s[k] == b'0' {
            k += 1;
        }
        &s[k..]
    }

    fn as_i32(o: core::cmp::Ordering) -> i32 {
        match o {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "uint".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![
                    CollationSpec { id: CID_UINT, name: "uint".to_string() },
                ],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    /// No scalars exported, but the `collating` world requires the
    /// scalar-function export anyway. Always errors  the host
    /// won't call it because no scalar names are advertised.
    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err(format!("uint: no scalars declared (func_id={func_id})"))
        }
    }

    impl CollationGuest for Ext {
        fn compare(collation_id: u64, a: String, b: String) -> i32 {
            match collation_id {
                CID_UINT => uint_compare(&a, &b),
                _ => 0,  // unknown collation id  treat as equal
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
