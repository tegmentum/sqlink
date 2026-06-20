//! Roaring bitmap scalars (PLAN-more-extensions-2.md  6).
//!
//! Roaring bitmaps give exact-set membership at scale, with O(1)
//! cardinality and fast set-algebra (union / intersection /
//! difference / symmetric-difference). They complement the
//! probabilistic structures already in the catalog (bloom,
//! hyperloglog, count-min)  use bloom when you can tolerate false
//! positives and want a fixed-size filter; use roaring when the set
//! is sparse-but-not-too-sparse and you want exact answers.
//!
//! Values are u32 per the Roaring spec. SQL i64 inputs that fit in
//! u32 are coerced silently; out-of-range inputs (negative, or
//! > u32::MAX) return an error rather than wrapping.
//!
//! The BLOB returned by every constructor / mutator is the portable
//! Roaring serialization spec (CRoaring-compatible byte format),
//! produced by `RoaringBitmap::serialize_into`. That means a BLOB
//! produced here can be loaded directly by CRoaring / pyroaring /
//! the Java reference impl, and vice versa. `rb_serialize` and
//! `rb_deserialize` are explicitly named identity round-trips that
//! exist to document that boundary in SQL (and to give callers a
//! place to validate that a hand-crafted blob round-trips cleanly).
//!
//! Function surface:
//!
//!   rb_new()                          -> BLOB
//!   rb_from_array(json_array)         -> BLOB
//!   rb_from_range(lo, hi)             -> BLOB  (inclusive)
//!   rb_to_array(rb)                   -> TEXT  (sorted JSON array)
//!   rb_cardinality(rb)                -> INTEGER
//!   rb_contains(rb, value)            -> INTEGER (0/1)
//!   rb_add(rb, value)                 -> BLOB
//!   rb_remove(rb, value)              -> BLOB
//!   rb_union(a, b)                    -> BLOB
//!   rb_intersection(a, b)             -> BLOB
//!   rb_difference(a, b)               -> BLOB
//!   rb_symmetric_difference(a, b)     -> BLOB
//!   rb_serialize(rb)                  -> BLOB  (portable Roaring spec)
//!   rb_deserialize(blob)              -> BLOB  (validates + canonicalises)
//!   rb_version()                      -> TEXT

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use roaring::RoaringBitmap;

// ─────────────── helpers ───────────────

/// Coerce a signed SQL i64 to u32. Negative or > u32::MAX errors,
/// so callers cannot accidentally insert a wraparound value. The
/// Roaring spec is strictly u32, and silently wrapping would
/// corrupt the set semantics.
pub fn coerce_u32(n: i64, fname: &str) -> Result<u32, String> {
    if (0..=(u32::MAX as i64)).contains(&n) {
        Ok(n as u32)
    } else {
        Err(format!(
            "{fname}: value {n} out of u32 range (0..=4294967295)"
        ))
    }
}

/// Decode a Roaring bitmap from its portable byte blob.
pub fn decode(blob: &[u8]) -> Result<RoaringBitmap, String> {
    RoaringBitmap::deserialize_from(blob).map_err(|e| format!("roaring decode: {e}"))
}

/// Encode a Roaring bitmap to its portable byte blob.
pub fn encode(rb: &RoaringBitmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(rb.serialized_size());
    // `serialize_into` writes via the std `Write` impl on Vec<u8>;
    // it cannot fail for an in-memory growing buffer.
    rb.serialize_into(&mut out)
        .expect("roaring serialize to Vec<u8> is infallible");
    out
}

// ─────────────── constructors ───────────────

pub fn rb_new() -> Vec<u8> {
    encode(&RoaringBitmap::new())
}

/// JSON array of unsigned ints, e.g. `[1, 2, 3, 4294967295]`.
/// Non-array, non-integer, or out-of-range entries error.
pub fn rb_from_array(json_array: &str) -> Result<Vec<u8>, String> {
    let v: serde_json::Value = serde_json::from_str(json_array)
        .map_err(|e| format!("rb_from_array: invalid JSON: {e}"))?;
    let arr = v
        .as_array()
        .ok_or_else(|| "rb_from_array: input must be a JSON array".to_string())?;
    let mut rb = RoaringBitmap::new();
    for (i, entry) in arr.iter().enumerate() {
        let n = entry
            .as_i64()
            .ok_or_else(|| format!("rb_from_array: element {i} is not an integer: {entry}"))?;
        let u = coerce_u32(n, "rb_from_array")?;
        rb.insert(u);
    }
    Ok(encode(&rb))
}

/// Inclusive `[lo, hi]` range. lo > hi yields an empty bitmap, which
/// matches Python's `range`-style "empty when reversed" intuition
/// and never errors.
pub fn rb_from_range(lo: i64, hi: i64) -> Result<Vec<u8>, String> {
    let l = coerce_u32(lo, "rb_from_range")?;
    let h = coerce_u32(hi, "rb_from_range")?;
    let mut rb = RoaringBitmap::new();
    if l <= h {
        // insert_range takes a half-open RangeBounds<u32>. To include
        // h we want l..=h; `l..h+1` would overflow at h == u32::MAX,
        // so route through the inclusive range form instead.
        rb.insert_range(l..=h);
    }
    Ok(encode(&rb))
}

// ─────────────── readers ───────────────

pub fn rb_to_array(blob: &[u8]) -> Result<String, String> {
    let rb = decode(blob)?;
    // RoaringBitmap::iter() yields in ascending order, so the output
    // array is sorted. Build the string by hand to skip the
    // serde_json::Value indirection.
    use core::fmt::Write;
    let mut s = String::with_capacity(rb.len() as usize * 4 + 2);
    s.push('[');
    let mut first = true;
    for v in rb.iter() {
        if !first {
            s.push(',');
        }
        first = false;
        write!(&mut s, "{v}").unwrap();
    }
    s.push(']');
    Ok(s)
}

pub fn rb_cardinality(blob: &[u8]) -> Result<i64, String> {
    Ok(decode(blob)?.len() as i64)
}

pub fn rb_contains(blob: &[u8], value: i64) -> Result<i64, String> {
    let rb = decode(blob)?;
    // Negative or out-of-range "contains?" is a definite no  rather
    // than erroring like the mutators, fall through to 0. Matches
    // bloom_might_contain's "any input lookup is well-defined" stance.
    if !(0..=(u32::MAX as i64)).contains(&value) {
        return Ok(0);
    }
    Ok(rb.contains(value as u32) as i64)
}

// ─────────────── mutators ───────────────

pub fn rb_add(blob: &[u8], value: i64) -> Result<Vec<u8>, String> {
    let v = coerce_u32(value, "rb_add")?;
    let mut rb = decode(blob)?;
    rb.insert(v);
    Ok(encode(&rb))
}

pub fn rb_remove(blob: &[u8], value: i64) -> Result<Vec<u8>, String> {
    let v = coerce_u32(value, "rb_remove")?;
    let mut rb = decode(blob)?;
    rb.remove(v);
    Ok(encode(&rb))
}

// ─────────────── set algebra ───────────────

pub fn rb_union(a: &[u8], b: &[u8]) -> Result<Vec<u8>, String> {
    Ok(encode(&(decode(a)? | decode(b)?)))
}

pub fn rb_intersection(a: &[u8], b: &[u8]) -> Result<Vec<u8>, String> {
    Ok(encode(&(decode(a)? & decode(b)?)))
}

pub fn rb_difference(a: &[u8], b: &[u8]) -> Result<Vec<u8>, String> {
    Ok(encode(&(decode(a)? - decode(b)?)))
}

pub fn rb_symmetric_difference(a: &[u8], b: &[u8]) -> Result<Vec<u8>, String> {
    Ok(encode(&(decode(a)? ^ decode(b)?)))
}

// ─────────────── identity round-trip ───────────────

/// `rb_serialize` and `rb_deserialize` round-trip through the
/// portable Roaring spec  the BLOB shape used everywhere already
/// IS that spec, so the body is just decode + re-encode. The point
/// is to (a) document the wire-format boundary in SQL and (b) give
/// callers a place to validate / canonicalise a hand-built blob.
pub fn rb_serialize(blob: &[u8]) -> Result<Vec<u8>, String> {
    Ok(encode(&decode(blob)?))
}

pub fn rb_deserialize(blob: &[u8]) -> Result<Vec<u8>, String> {
    Ok(encode(&decode(blob)?))
}

// ─────────────── wasm component export ───────────────

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

    // ---- Function IDs ----
    // Allocate one constant per scalar. Keep IDs stable: changing
    // a u64 here is an ABI break for callers that resolved by id.
    const FID_NEW: u64 = 1;
    const FID_FROM_ARRAY: u64 = 2;
    const FID_FROM_RANGE: u64 = 3;
    const FID_TO_ARRAY: u64 = 4;
    const FID_CARDINALITY: u64 = 5;
    const FID_CONTAINS: u64 = 6;
    const FID_ADD: u64 = 7;
    const FID_REMOVE: u64 = 8;
    const FID_UNION: u64 = 9;
    const FID_INTERSECTION: u64 = 10;
    const FID_DIFFERENCE: u64 = 11;
    const FID_SYMMETRIC_DIFFERENCE: u64 = 12;
    const FID_SERIALIZE: u64 = 13;
    const FID_DESERIALIZE: u64 = 14;
    const FID_VERSION: u64 = 15;

    struct Ext;

    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
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
                name: "roaring".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NEW, "rb_new", 0, det),
                    s(FID_FROM_ARRAY, "rb_from_array", 1, det),
                    s(FID_FROM_RANGE, "rb_from_range", 2, det),
                    s(FID_TO_ARRAY, "rb_to_array", 1, det),
                    s(FID_CARDINALITY, "rb_cardinality", 1, det),
                    s(FID_CONTAINS, "rb_contains", 2, det),
                    s(FID_ADD, "rb_add", 2, det),
                    s(FID_REMOVE, "rb_remove", 2, det),
                    s(FID_UNION, "rb_union", 2, det),
                    s(FID_INTERSECTION, "rb_intersection", 2, det),
                    s(FID_DIFFERENCE, "rb_difference", 2, det),
                    s(FID_SYMMETRIC_DIFFERENCE, "rb_symmetric_difference", 2, det),
                    s(FID_SERIALIZE, "rb_serialize", 1, det),
                    s(FID_DESERIALIZE, "rb_deserialize", 1, det),
                    s(FID_VERSION, "rb_version", 0, det),
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
                FID_NEW => Ok(SqlValue::Blob(super::rb_new())),
                FID_FROM_ARRAY => {
                    let j = arg_text(&args, 0, "rb_from_array")?;
                    super::rb_from_array(&j).map(SqlValue::Blob)
                }
                FID_FROM_RANGE => {
                    let lo = arg_int(&args, 0, "rb_from_range")?;
                    let hi = arg_int(&args, 1, "rb_from_range")?;
                    super::rb_from_range(lo, hi).map(SqlValue::Blob)
                }
                FID_TO_ARRAY => {
                    let b = arg_blob(&args, 0, "rb_to_array")?;
                    super::rb_to_array(&b).map(SqlValue::Text)
                }
                FID_CARDINALITY => {
                    let b = arg_blob(&args, 0, "rb_cardinality")?;
                    super::rb_cardinality(&b).map(SqlValue::Integer)
                }
                FID_CONTAINS => {
                    let b = arg_blob(&args, 0, "rb_contains")?;
                    let v = arg_int(&args, 1, "rb_contains")?;
                    super::rb_contains(&b, v).map(SqlValue::Integer)
                }
                FID_ADD => {
                    let b = arg_blob(&args, 0, "rb_add")?;
                    let v = arg_int(&args, 1, "rb_add")?;
                    super::rb_add(&b, v).map(SqlValue::Blob)
                }
                FID_REMOVE => {
                    let b = arg_blob(&args, 0, "rb_remove")?;
                    let v = arg_int(&args, 1, "rb_remove")?;
                    super::rb_remove(&b, v).map(SqlValue::Blob)
                }
                FID_UNION => {
                    let a = arg_blob(&args, 0, "rb_union")?;
                    let b = arg_blob(&args, 1, "rb_union")?;
                    super::rb_union(&a, &b).map(SqlValue::Blob)
                }
                FID_INTERSECTION => {
                    let a = arg_blob(&args, 0, "rb_intersection")?;
                    let b = arg_blob(&args, 1, "rb_intersection")?;
                    super::rb_intersection(&a, &b).map(SqlValue::Blob)
                }
                FID_DIFFERENCE => {
                    let a = arg_blob(&args, 0, "rb_difference")?;
                    let b = arg_blob(&args, 1, "rb_difference")?;
                    super::rb_difference(&a, &b).map(SqlValue::Blob)
                }
                FID_SYMMETRIC_DIFFERENCE => {
                    let a = arg_blob(&args, 0, "rb_symmetric_difference")?;
                    let b = arg_blob(&args, 1, "rb_symmetric_difference")?;
                    super::rb_symmetric_difference(&a, &b).map(SqlValue::Blob)
                }
                FID_SERIALIZE => {
                    let b = arg_blob(&args, 0, "rb_serialize")?;
                    super::rb_serialize(&b).map(SqlValue::Blob)
                }
                FID_DESERIALIZE => {
                    let b = arg_blob(&args, 0, "rb_deserialize")?;
                    super::rb_deserialize(&b).map(SqlValue::Blob)
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("roaring: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
