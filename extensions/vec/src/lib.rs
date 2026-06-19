//! sqlite-vec scalar surface  vector similarity primitives.
//!
//! Storage: packed little-endian f32 BLOBs. Dimension is
//! implied by `blob.len() / 4` (must be a multiple of 4).
//! Distance kernels iterate two slices of equal length and
//! return f64; the L1/L2/cosine forms cover the common cases
//! for embedded k-NN.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug)]
pub enum VecError {
    BadBlobLength(usize),
    DimMismatch(usize, usize),
    ParseJson(String),
    JsonNotArray,
    JsonElement,
    Empty,
}

impl core::fmt::Display for VecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            VecError::BadBlobLength(n) => {
                write!(f, "vector blob length {n} is not a multiple of 4 (f32)")
            }
            VecError::DimMismatch(a, b) => {
                write!(f, "vector dim mismatch: {a} vs {b}")
            }
            VecError::ParseJson(e) => write!(f, "JSON parse: {e}"),
            VecError::JsonNotArray => write!(f, "JSON is not an array"),
            VecError::JsonElement => write!(f, "JSON element is not a finite number"),
            VecError::Empty => write!(f, "empty vector"),
        }
    }
}

pub fn from_blob(b: &[u8]) -> Result<Vec<f32>, VecError> {
    if b.len() % 4 != 0 {
        return Err(VecError::BadBlobLength(b.len()));
    }
    let n = b.len() / 4;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let bytes = [b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]];
        out.push(f32::from_le_bytes(bytes));
    }
    Ok(out)
}

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub fn from_json(s: &str) -> Result<Vec<f32>, VecError> {
    let v: serde_json::Value =
        serde_json::from_str(s).map_err(|e| VecError::ParseJson(format!("{e}")))?;
    let arr = v.as_array().ok_or(VecError::JsonNotArray)?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let f = item.as_f64().ok_or(VecError::JsonElement)?;
        out.push(f as f32);
    }
    Ok(out)
}

pub fn to_json(v: &[f32]) -> String {
    let arr: Vec<serde_json::Value> = v
        .iter()
        .map(|x| {
            serde_json::Number::from_f64(*x as f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        })
        .collect();
    serde_json::Value::Array(arr).to_string()
}

pub fn l1(a: &[f32], b: &[f32]) -> Result<f64, VecError> {
    if a.len() != b.len() {
        return Err(VecError::DimMismatch(a.len(), b.len()));
    }
    let mut s = 0.0f64;
    for i in 0..a.len() {
        s += (a[i] as f64 - b[i] as f64).abs();
    }
    Ok(s)
}

pub fn l2(a: &[f32], b: &[f32]) -> Result<f64, VecError> {
    if a.len() != b.len() {
        return Err(VecError::DimMismatch(a.len(), b.len()));
    }
    let mut s = 0.0f64;
    for i in 0..a.len() {
        let d = a[i] as f64 - b[i] as f64;
        s += d * d;
    }
    Ok(s.sqrt())
}

/// Cosine **distance**: `1 - (a.b / (|a| |b|))`. Zero vectors
/// return f64::NAN per the upstream sqlite-vec convention
/// the geometry is undefined when either norm is zero.
pub fn cosine(a: &[f32], b: &[f32]) -> Result<f64, VecError> {
    if a.len() != b.len() {
        return Err(VecError::DimMismatch(a.len(), b.len()));
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for i in 0..a.len() {
        let x = a[i] as f64;
        let y = b[i] as f64;
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return Ok(f64::NAN);
    }
    Ok(1.0 - dot / (na.sqrt() * nb.sqrt()))
}

pub fn add(a: &[f32], b: &[f32]) -> Result<Vec<f32>, VecError> {
    if a.len() != b.len() {
        return Err(VecError::DimMismatch(a.len(), b.len()));
    }
    Ok(a.iter().zip(b.iter()).map(|(x, y)| x + y).collect())
}

pub fn sub(a: &[f32], b: &[f32]) -> Result<Vec<f32>, VecError> {
    if a.len() != b.len() {
        return Err(VecError::DimMismatch(a.len(), b.len()));
    }
    Ok(a.iter().zip(b.iter()).map(|(x, y)| x - y).collect())
}

/// L2-normalize. Zero vectors stay zero (no NaN explosion).
pub fn normalize(v: &[f32]) -> Vec<f32> {
    let n: f64 = v.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>().sqrt();
    if n == 0.0 {
        return v.to_vec();
    }
    let inv = (1.0 / n) as f32;
    v.iter().map(|x| x * inv).collect()
}

pub fn slice(v: &[f32], start: i64, end: i64) -> Vec<f32> {
    let n = v.len() as i64;
    let s = start.clamp(0, n) as usize;
    let e = end.clamp(0, n) as usize;
    if s >= e {
        return Vec::new();
    }
    v[s..e].to_vec()
}

/// Binary quantization: 1 bit per dim, MSB-first per byte.
/// `sign(x) >= 0`  bit set. Useful for cheap hash-table prefilter
/// before a precise distance pass.
pub fn quantize_binary(v: &[f32]) -> Vec<u8> {
    let n_bytes = v.len().div_ceil(8);
    let mut out = alloc::vec![0u8; n_bytes];
    for (i, x) in v.iter().enumerate() {
        if *x >= 0.0 {
            out[i / 8] |= 1u8 << (7 - (i % 8));
        }
    }
    out
}

/// int8 quantization: scale by `127 / max(|x|)` and round.
/// Empty/zero vectors produce an all-zero output of the same
/// length; that's reversible to zero-on-decode.
pub fn quantize_int8(v: &[f32]) -> Vec<u8> {
    if v.is_empty() {
        return Vec::new();
    }
    let m = v.iter().fold(0.0f32, |a, x| a.max(x.abs()));
    if m == 0.0 {
        return alloc::vec![0i8 as u8; v.len()];
    }
    let scale = 127.0 / m;
    v.iter()
        .map(|x| {
            let q = (x * scale).round().clamp(-128.0, 127.0);
            q as i8 as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn blob_roundtrip() {
        let v = alloc::vec![1.0f32, -2.5, 3.14159, 0.0];
        let b = to_blob(&v);
        assert_eq!(b.len(), 16);
        let back = from_blob(&b).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn json_roundtrip() {
        let v = alloc::vec![1.0f32, 2.0, 3.0];
        let s = to_json(&v);
        let back = from_json(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn from_blob_bad_length() {
        assert!(matches!(
            from_blob(&[0u8; 7]),
            Err(VecError::BadBlobLength(7))
        ));
    }

    #[test]
    fn distance_l1_l2_known() {
        let a = alloc::vec![1.0f32, 2.0, 3.0];
        let b = alloc::vec![4.0f32, 6.0, 8.0];
        assert!(approx(l1(&a, &b).unwrap(), 3.0 + 4.0 + 5.0, 1e-6));
        assert!(approx(l2(&a, &b).unwrap(), (9.0f64 + 16.0 + 25.0).sqrt(), 1e-6));
    }

    #[test]
    fn distance_cosine_orthogonal() {
        let a = alloc::vec![1.0f32, 0.0];
        let b = alloc::vec![0.0f32, 1.0];
        assert!(approx(cosine(&a, &b).unwrap(), 1.0, 1e-6));
        // Identical vectors  distance 0.
        assert!(approx(cosine(&a, &a).unwrap(), 0.0, 1e-6));
        // Anti-parallel  distance 2.
        let c = alloc::vec![-1.0f32, 0.0];
        assert!(approx(cosine(&a, &c).unwrap(), 2.0, 1e-6));
        // Zero vector  NaN.
        let z = alloc::vec![0.0f32, 0.0];
        assert!(cosine(&a, &z).unwrap().is_nan());
    }

    #[test]
    fn dim_mismatch_errors() {
        let a = alloc::vec![1.0f32];
        let b = alloc::vec![1.0f32, 2.0];
        assert!(matches!(l1(&a, &b), Err(VecError::DimMismatch(1, 2))));
        assert!(matches!(add(&a, &b), Err(VecError::DimMismatch(1, 2))));
    }

    #[test]
    fn normalize_then_l2_is_one() {
        let v = alloc::vec![3.0f32, 4.0];
        let n = normalize(&v);
        let norm: f64 = n.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>().sqrt();
        assert!(approx(norm, 1.0, 1e-6));
    }

    #[test]
    fn slice_half_open() {
        let v = alloc::vec![1.0f32, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(slice(&v, 1, 4), alloc::vec![2.0f32, 3.0, 4.0]);
        assert_eq!(slice(&v, 0, 0), Vec::<f32>::new());
        assert_eq!(slice(&v, -1, 10), alloc::vec![1.0f32, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn quantize_binary_packs_sign() {
        let v: Vec<f32> = (0..8).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        // 1 0 1 0 1 0 1 0  0b10101010 = 0xAA
        assert_eq!(quantize_binary(&v), alloc::vec![0xAAu8]);
    }

    #[test]
    fn quantize_int8_scales_to_max() {
        let v = alloc::vec![1.0f32, -0.5, 0.25];
        let q = quantize_int8(&v);
        assert_eq!(q.len(), 3);
        // max abs = 1.0, scale = 127, so values round to ±127, ±64ish, ±32ish.
        let signed: Vec<i8> = q.iter().map(|b| *b as i8).collect();
        assert_eq!(signed[0], 127);
        assert!((signed[1] - (-64)).abs() <= 1);
        assert!((signed[2] - 32).abs() <= 1);
    }
}

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

    const FID_VEC_F32: u64 = 1;
    const FID_VEC_TO_JSON: u64 = 2;
    const FID_VEC_LENGTH: u64 = 3;
    const FID_VEC_TYPE: u64 = 4;
    const FID_VEC_VERSION: u64 = 5;
    const FID_VEC_DISTANCE_L1: u64 = 6;
    const FID_VEC_DISTANCE_L2: u64 = 7;
    const FID_VEC_DISTANCE_COSINE: u64 = 8;
    const FID_VEC_ADD: u64 = 9;
    const FID_VEC_SUB: u64 = 10;
    const FID_VEC_NORMALIZE: u64 = 11;
    const FID_VEC_SLICE: u64 = 12;
    const FID_VEC_QUANTIZE_BINARY: u64 = 13;
    const FID_VEC_QUANTIZE_INT8: u64 = 14;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let d = FunctionFlags::DETERMINISTIC;
            let n = FunctionFlags::empty();
            let s = |id, name: &str, num_args: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: f,
            };
            Manifest {
                name: "vec".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VEC_F32, "vec_f32", 1, d),
                    s(FID_VEC_TO_JSON, "vec_to_json", 1, d),
                    s(FID_VEC_LENGTH, "vec_length", 1, d),
                    s(FID_VEC_TYPE, "vec_type", 1, d),
                    // vec_version() is "non-deterministic" only in
                    // the sense that its result depends on which
                    // build is loaded  it's stable within a run.
                    s(FID_VEC_VERSION, "vec_version", 0, n),
                    s(FID_VEC_DISTANCE_L1, "vec_distance_l1", 2, d),
                    s(FID_VEC_DISTANCE_L2, "vec_distance_l2", 2, d),
                    s(FID_VEC_DISTANCE_COSINE, "vec_distance_cosine", 2, d),
                    s(FID_VEC_ADD, "vec_add", 2, d),
                    s(FID_VEC_SUB, "vec_sub", 2, d),
                    s(FID_VEC_NORMALIZE, "vec_normalize", 1, d),
                    s(FID_VEC_SLICE, "vec_slice", 3, d),
                    s(FID_VEC_QUANTIZE_BINARY, "vec_quantize_binary", 1, d),
                    s(FID_VEC_QUANTIZE_INT8, "vec_quantize_int8", 1, d),
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

    /// Accept BLOB (already-packed f32) or TEXT (JSON array).
    fn parse_vec(v: &SqlValue, fname: &str) -> Result<Vec<f32>, String> {
        match v {
            SqlValue::Blob(b) => super::from_blob(b).map_err(|e| format!("{fname}: {e}")),
            SqlValue::Text(s) => super::from_json(s).map_err(|e| format!("{fname}: {e}")),
            SqlValue::Null => Err(format!("{fname}: null arg")),
            _ => Err(format!("{fname}: expected vector BLOB or JSON TEXT")),
        }
    }

    fn arg_i64(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(f)) => Ok(*f as i64),
            _ => Err(format!("{fname}: integer expected at arg {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VEC_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_VEC_F32 => {
                    let v = parse_vec(args.first().ok_or("missing arg 0")?, "vec_f32")?;
                    Ok(SqlValue::Blob(super::to_blob(&v)))
                }
                FID_VEC_TO_JSON => {
                    let v = parse_vec(args.first().ok_or("missing arg 0")?, "vec_to_json")?;
                    Ok(SqlValue::Text(super::to_json(&v)))
                }
                FID_VEC_LENGTH => {
                    let v = parse_vec(args.first().ok_or("missing arg 0")?, "vec_length")?;
                    Ok(SqlValue::Integer(v.len() as i64))
                }
                FID_VEC_TYPE => {
                    // Only float32 is supported today; the type
                    // tag exists so callers can branch when int8
                    // / bit storage lands later.
                    let _ = parse_vec(args.first().ok_or("missing arg 0")?, "vec_type")?;
                    Ok(SqlValue::Text("float32".to_string()))
                }
                FID_VEC_DISTANCE_L1 => {
                    let a = parse_vec(&args[0], "vec_distance_l1")?;
                    let b = parse_vec(&args[1], "vec_distance_l1")?;
                    super::l1(&a, &b)
                        .map(SqlValue::Real)
                        .map_err(|e| format!("vec_distance_l1: {e}"))
                }
                FID_VEC_DISTANCE_L2 => {
                    let a = parse_vec(&args[0], "vec_distance_l2")?;
                    let b = parse_vec(&args[1], "vec_distance_l2")?;
                    super::l2(&a, &b)
                        .map(SqlValue::Real)
                        .map_err(|e| format!("vec_distance_l2: {e}"))
                }
                FID_VEC_DISTANCE_COSINE => {
                    let a = parse_vec(&args[0], "vec_distance_cosine")?;
                    let b = parse_vec(&args[1], "vec_distance_cosine")?;
                    super::cosine(&a, &b)
                        .map(SqlValue::Real)
                        .map_err(|e| format!("vec_distance_cosine: {e}"))
                }
                FID_VEC_ADD => {
                    let a = parse_vec(&args[0], "vec_add")?;
                    let b = parse_vec(&args[1], "vec_add")?;
                    super::add(&a, &b)
                        .map(|v| SqlValue::Blob(super::to_blob(&v)))
                        .map_err(|e| format!("vec_add: {e}"))
                }
                FID_VEC_SUB => {
                    let a = parse_vec(&args[0], "vec_sub")?;
                    let b = parse_vec(&args[1], "vec_sub")?;
                    super::sub(&a, &b)
                        .map(|v| SqlValue::Blob(super::to_blob(&v)))
                        .map_err(|e| format!("vec_sub: {e}"))
                }
                FID_VEC_NORMALIZE => {
                    let v = parse_vec(&args[0], "vec_normalize")?;
                    Ok(SqlValue::Blob(super::to_blob(&super::normalize(&v))))
                }
                FID_VEC_SLICE => {
                    let v = parse_vec(&args[0], "vec_slice")?;
                    let s = arg_i64(&args, 1, "vec_slice")?;
                    let e = arg_i64(&args, 2, "vec_slice")?;
                    Ok(SqlValue::Blob(super::to_blob(&super::slice(&v, s, e))))
                }
                FID_VEC_QUANTIZE_BINARY => {
                    let v = parse_vec(&args[0], "vec_quantize_binary")?;
                    Ok(SqlValue::Blob(super::quantize_binary(&v)))
                }
                FID_VEC_QUANTIZE_INT8 => {
                    let v = parse_vec(&args[0], "vec_quantize_int8")?;
                    Ok(SqlValue::Blob(super::quantize_int8(&v)))
                }
                other => Err(format!("vec: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
