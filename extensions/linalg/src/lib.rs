//! Dense linear algebra extension for SQLite (PLAN-more-extensions-5.md
//! #5). Matrices flow as JSON 2D arrays of f64; the dense f64 path of
//! `nalgebra` 0.33 backs every op. NULL on malformed JSON, shape
//! mismatch, or singular system -- no panics, no errors raised.
//!
//! Function surface:
//!
//!   la_zeros(rows, cols)        -> text   (JSON 2D array of 0.0)
//!   la_eye(n)                   -> text   (identity matrix as JSON)
//!   la_transpose(m_json)        -> text
//!   la_add(a_json, b_json)      -> text
//!   la_sub(a_json, b_json)      -> text
//!   la_mul(a_json, b_json)      -> text   (matrix multiply, NOT elementwise)
//!   la_scale(m_json, k)         -> text
//!   la_det(m_json)              -> real   (NULL if non-square)
//!   la_inverse(m_json)          -> text   (NULL if singular / non-square)
//!   la_solve(a_json, b_json)    -> text   (Ax = b; b may be vector or N×k matrix)
//!   la_rank(m_json)             -> integer
//!   la_eigvals(m_json)          -> text   (JSON array of {re, im})
//!   la_trace(m_json)            -> real   (NULL if non-square)
//!   la_norm(m_json[, kind])     -> real   ('fro'/'l1'/'linf'; default 'fro')
//!   la_shape(m_json)            -> text   (JSON [rows, cols])
//!   linalg_version()            -> text
//!
//! JSON shape rules:
//!   - Matrix: array-of-arrays, rectangular, every cell finite f64.
//!     Ragged or non-numeric entries -> NULL.
//!   - Vector (la_solve b): either 1D array `[1,2,3]` (interpreted as
//!     column vector) or 2D N×k matrix; result mirrors the input form.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use nalgebra::{Complex, DMatrix};
use serde_json::Value as JsonValue;

// ---------- JSON <-> DMatrix codec ----------

/// Parse a JSON 2D array (rectangular, all cells finite f64) into a
/// row-major `DMatrix<f64>`. Returns `None` on any structural defect:
///   - not an array, or array of non-arrays
///   - ragged rows
///   - empty outer or inner array
///   - non-numeric / non-finite cells
fn parse_matrix(s: &str) -> Option<DMatrix<f64>> {
    let v: JsonValue = serde_json::from_str(s).ok()?;
    let rows_json = v.as_array()?;
    let nrows = rows_json.len();
    if nrows == 0 {
        return None;
    }
    let first = rows_json[0].as_array()?;
    let ncols = first.len();
    if ncols == 0 {
        return None;
    }
    let mut data: Vec<f64> = Vec::with_capacity(nrows * ncols);
    for row in rows_json {
        let r = row.as_array()?;
        if r.len() != ncols {
            return None;
        }
        for cell in r {
            let x = cell.as_f64()?;
            if !x.is_finite() {
                return None;
            }
            data.push(x);
        }
    }
    // nalgebra DMatrix::from_row_slice expects row-major data with the
    // rows × cols layout we just built.
    Some(DMatrix::from_row_slice(nrows, ncols, &data))
}

/// Parse either a 1D JSON array `[1,2,3]` or a 2D `[[1],[2],[3]]` /
/// `[[1,2],[3,4]]` into a `DMatrix<f64>`. Returns `(matrix, was_1d)`
/// so the result formatter can mirror the input shape. None on any
/// structural defect.
fn parse_vec_or_matrix(s: &str) -> Option<(DMatrix<f64>, bool)> {
    let v: JsonValue = serde_json::from_str(s).ok()?;
    let arr = v.as_array()?;
    if arr.is_empty() {
        return None;
    }
    // 1D array: every element is a scalar.
    if arr.iter().all(|x| !x.is_array()) {
        let mut data = Vec::with_capacity(arr.len());
        for cell in arr {
            let x = cell.as_f64()?;
            if !x.is_finite() {
                return None;
            }
            data.push(x);
        }
        // Column vector (N × 1).
        return Some((DMatrix::from_column_slice(data.len(), 1, &data), true));
    }
    // Otherwise parse as a 2D matrix.
    let m = parse_matrix(s)?;
    Some((m, false))
}

/// Format a finite f64 as JSON. Integer-valued floats render without
/// a decimal point so smoke.expected lines stay readable (e.g. "1"
/// rather than "1.0"). Non-finite -> None (caller's responsibility to
/// translate to NULL).
fn fmt_num(x: f64) -> Option<String> {
    if !x.is_finite() {
        return None;
    }
    // Treat -0.0 as 0.0 to keep round-trips clean.
    let x = if x == 0.0 { 0.0 } else { x };
    if x.fract() == 0.0 && x.abs() < 1e16 {
        // Render as integer.
        Some(format!("{}", x as i64))
    } else {
        // serde_json's default f64 formatter, via JsonValue.
        Some(JsonValue::from(x).to_string())
    }
}

/// Serialize a `DMatrix<f64>` as a JSON 2D array. Returns `None` if
/// any cell is non-finite (NaN / Inf escape unhandled -> NULL).
fn fmt_matrix(m: &DMatrix<f64>) -> Option<String> {
    let nrows = m.nrows();
    let ncols = m.ncols();
    let mut out = String::with_capacity(2 + nrows * ncols * 8);
    out.push('[');
    for i in 0..nrows {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for j in 0..ncols {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&fmt_num(m[(i, j)])?);
        }
        out.push(']');
    }
    out.push(']');
    Some(out)
}

/// Serialize a column-vector `DMatrix<f64>` (N × 1) as a 1D JSON array
/// `[a,b,c]`. None on any non-finite cell.
fn fmt_column_as_1d(m: &DMatrix<f64>) -> Option<String> {
    assert_eq!(m.ncols(), 1, "fmt_column_as_1d expects N×1");
    let mut out = String::with_capacity(2 + m.nrows() * 8);
    out.push('[');
    for i in 0..m.nrows() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&fmt_num(m[(i, 0)])?);
    }
    out.push(']');
    Some(out)
}

// ---------- public scalar ops ----------

/// Zero matrix of the given size. NULL on non-positive dims or on
/// ridiculous sizes (cap at 10000 cells -- the JSON would dwarf the
/// rest of the catalog otherwise).
pub fn la_zeros(rows: i64, cols: i64) -> Option<String> {
    if rows <= 0 || cols <= 0 {
        return None;
    }
    if rows.saturating_mul(cols) > 10_000 {
        return None;
    }
    let m = DMatrix::<f64>::zeros(rows as usize, cols as usize);
    fmt_matrix(&m)
}

/// n × n identity. NULL on n <= 0 or n > 100.
pub fn la_eye(n: i64) -> Option<String> {
    if n <= 0 || n > 100 {
        return None;
    }
    let m = DMatrix::<f64>::identity(n as usize, n as usize);
    fmt_matrix(&m)
}

pub fn la_transpose(s: &str) -> Option<String> {
    let m = parse_matrix(s)?;
    fmt_matrix(&m.transpose())
}

pub fn la_add(a: &str, b: &str) -> Option<String> {
    let ma = parse_matrix(a)?;
    let mb = parse_matrix(b)?;
    if ma.shape() != mb.shape() {
        return None;
    }
    fmt_matrix(&(ma + mb))
}

pub fn la_sub(a: &str, b: &str) -> Option<String> {
    let ma = parse_matrix(a)?;
    let mb = parse_matrix(b)?;
    if ma.shape() != mb.shape() {
        return None;
    }
    fmt_matrix(&(ma - mb))
}

pub fn la_mul(a: &str, b: &str) -> Option<String> {
    let ma = parse_matrix(a)?;
    let mb = parse_matrix(b)?;
    // (m × n) × (n × p) = m × p.
    if ma.ncols() != mb.nrows() {
        return None;
    }
    fmt_matrix(&(ma * mb))
}

pub fn la_scale(s: &str, k: f64) -> Option<String> {
    if !k.is_finite() {
        return None;
    }
    let m = parse_matrix(s)?;
    fmt_matrix(&(m * k))
}

pub fn la_det(s: &str) -> Option<f64> {
    let m = parse_matrix(s)?;
    if m.nrows() != m.ncols() {
        return None;
    }
    let d = m.determinant();
    if d.is_finite() {
        Some(d)
    } else {
        None
    }
}

/// Inverse. NULL on non-square or singular (LU decomposition reports
/// no inverse). `try_inverse` does the right thing here.
pub fn la_inverse(s: &str) -> Option<String> {
    let m = parse_matrix(s)?;
    if m.nrows() != m.ncols() {
        return None;
    }
    let inv = m.try_inverse()?;
    fmt_matrix(&inv)
}

/// Solve Ax = b. b may be a 1D JSON array (column vector) or a 2D
/// matrix; the result mirrors the input shape. NULL on shape mismatch
/// or singular A.
pub fn la_solve(a: &str, b: &str) -> Option<String> {
    let ma = parse_matrix(a)?;
    if ma.nrows() != ma.ncols() {
        return None;
    }
    let (mb, was_1d) = parse_vec_or_matrix(b)?;
    if mb.nrows() != ma.ncols() {
        return None;
    }
    // LU decomposition with partial pivoting -- the standard solver.
    let lu = ma.lu();
    let x = lu.solve(&mb)?;
    if was_1d {
        fmt_column_as_1d(&x)
    } else {
        fmt_matrix(&x)
    }
}

/// Rank via SVD with the default epsilon nalgebra exposes for the
/// matrix's element type. NULL only on malformed input.
pub fn la_rank(s: &str) -> Option<i64> {
    let m = parse_matrix(s)?;
    // f64::EPSILON is the standard cutoff for "singular value is
    // numerically zero"; multiplying by the largest singular value
    // would be more robust, but matches `np.linalg.matrix_rank(tol=
    // default)` exactly.
    let r = m.rank(f64::EPSILON);
    Some(r as i64)
}

/// Eigenvalues (possibly complex). Returns a JSON array of `{re, im}`
/// objects in nalgebra's natural order (no canonical sort -- callers
/// who need a sorted form can re-sort the JSON downstream).
pub fn la_eigvals(s: &str) -> Option<String> {
    let m = parse_matrix(s)?;
    if m.nrows() != m.ncols() {
        return None;
    }
    // Schur decomposition yields all eigenvalues (real & complex). For
    // a real square matrix `complex_eigenvalues()` returns Vec<Complex<f64>>.
    let eigs: Vec<Complex<f64>> = m.complex_eigenvalues().iter().copied().collect();
    let mut out = String::from("[");
    for (i, e) in eigs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let re = fmt_num(e.re)?;
        let im = fmt_num(e.im)?;
        out.push_str(&format!("{{\"re\":{re},\"im\":{im}}}"));
    }
    out.push(']');
    Some(out)
}

pub fn la_trace(s: &str) -> Option<f64> {
    let m = parse_matrix(s)?;
    if m.nrows() != m.ncols() {
        return None;
    }
    let t = m.trace();
    if t.is_finite() {
        Some(t)
    } else {
        None
    }
}

/// Matrix norm. Default 'fro' (Frobenius). 'l1' is the induced 1-norm
/// (max absolute column sum); 'linf' is the induced infinity-norm
/// (max absolute row sum). Unknown kind -> NULL.
pub fn la_norm(s: &str, kind: &str) -> Option<f64> {
    let m = parse_matrix(s)?;
    match kind {
        "fro" => {
            let n = m.norm();
            n.is_finite().then_some(n)
        }
        "l1" => {
            // max_j sum_i |m_ij|
            let mut best = 0.0_f64;
            for j in 0..m.ncols() {
                let mut s = 0.0_f64;
                for i in 0..m.nrows() {
                    s += m[(i, j)].abs();
                }
                if s > best {
                    best = s;
                }
            }
            best.is_finite().then_some(best)
        }
        "linf" => {
            // max_i sum_j |m_ij|
            let mut best = 0.0_f64;
            for i in 0..m.nrows() {
                let mut s = 0.0_f64;
                for j in 0..m.ncols() {
                    s += m[(i, j)].abs();
                }
                if s > best {
                    best = s;
                }
            }
            best.is_finite().then_some(best)
        }
        _ => None,
    }
}

pub fn la_shape(s: &str) -> Option<String> {
    let m = parse_matrix(s)?;
    Some(format!("[{},{}]", m.nrows(), m.ncols()))
}

// ─────────────── wasm32 wit-bindgen export ───────────────

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

    const FID_ZEROS: u64 = 1;
    const FID_EYE: u64 = 2;
    const FID_TRANSPOSE: u64 = 3;
    const FID_ADD: u64 = 4;
    const FID_SUB: u64 = 5;
    const FID_MUL: u64 = 6;
    const FID_SCALE: u64 = 7;
    const FID_DET: u64 = 8;
    const FID_INVERSE: u64 = 9;
    const FID_SOLVE: u64 = 10;
    const FID_RANK: u64 = 11;
    const FID_EIGVALS: u64 = 12;
    const FID_TRACE: u64 = 13;
    const FID_NORM: u64 = 14;
    const FID_SHAPE: u64 = 15;
    const FID_VERSION: u64 = 16;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize) -> Option<&str> {
        match args.get(i)? {
            SqlValue::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn arg_f64(args: &[SqlValue], i: usize) -> Option<f64> {
        match args.get(i)? {
            SqlValue::Integer(n) => Some(*n as f64),
            SqlValue::Real(r) => Some(*r),
            _ => None,
        }
    }

    fn arg_i64(args: &[SqlValue], i: usize) -> Option<i64> {
        match args.get(i)? {
            SqlValue::Integer(n) => Some(*n),
            SqlValue::Real(r) => {
                if r.fract() == 0.0 && r.is_finite() {
                    Some(*r as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// la_norm(m, [kind]) -- optional second arg, default "fro".
    fn arg_kind_default_fro(args: &[SqlValue], i: usize) -> Option<&str> {
        match args.get(i) {
            None | Some(SqlValue::Null) => Some("fro"),
            Some(SqlValue::Text(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    fn opt_text(r: Option<String>) -> SqlValue {
        match r {
            Some(s) => SqlValue::Text(s),
            None => SqlValue::Null,
            // PLAN-wit-value-extension.md Phase A: the sql-value variant
            // gained a wit-value arm; Phase B will replace this wildcard
            // with extension-specific decode/encode logic.
            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
        }
    }

    fn opt_real(r: Option<f64>) -> SqlValue {
        match r {
            Some(v) => SqlValue::Real(v),
            None => SqlValue::Null,
            // PLAN-wit-value-extension.md Phase A: the sql-value variant
            // gained a wit-value arm; Phase B will replace this wildcard
            // with extension-specific decode/encode logic.
            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
        }
    }

    fn opt_int(r: Option<i64>) -> SqlValue {
        match r {
            Some(v) => SqlValue::Integer(v),
            None => SqlValue::Null,
            // PLAN-wit-value-extension.md Phase A: the sql-value variant
            // gained a wit-value arm; Phase B will replace this wildcard
            // with extension-specific decode/encode logic.
            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
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
                name: "linalg".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ZEROS, "la_zeros", 2, det),
                    s(FID_EYE, "la_eye", 1, det),
                    s(FID_TRANSPOSE, "la_transpose", 1, det),
                    s(FID_ADD, "la_add", 2, det),
                    s(FID_SUB, "la_sub", 2, det),
                    s(FID_MUL, "la_mul", 2, det),
                    s(FID_SCALE, "la_scale", 2, det),
                    s(FID_DET, "la_det", 1, det),
                    s(FID_INVERSE, "la_inverse", 1, det),
                    s(FID_SOLVE, "la_solve", 2, det),
                    s(FID_RANK, "la_rank", 1, det),
                    s(FID_EIGVALS, "la_eigvals", 1, det),
                    s(FID_TRACE, "la_trace", 1, det),
                    // -1 => variadic (1 or 2 args; kind optional, defaults "fro").
                    s(FID_NORM, "la_norm", -1, det),
                    s(FID_SHAPE, "la_shape", 1, det),
                    s(FID_VERSION, "linalg_version", 0, det),
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
                preferred_prefix: Some("linalg".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.linalg".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_ZEROS => {
                    let r = match arg_i64(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    let c = match arg_i64(&args, 1) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_zeros(r, c)))
                }
                FID_EYE => {
                    let n = match arg_i64(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_eye(n)))
                }
                FID_TRANSPOSE => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_transpose(s)))
                }
                FID_ADD => {
                    let a = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    let b = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_add(a, b)))
                }
                FID_SUB => {
                    let a = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    let b = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_sub(a, b)))
                }
                FID_MUL => {
                    let a = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    let b = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_mul(a, b)))
                }
                FID_SCALE => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    let k = match arg_f64(&args, 1) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_scale(s, k)))
                }
                FID_DET => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_real(super::la_det(s)))
                }
                FID_INVERSE => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_inverse(s)))
                }
                FID_SOLVE => {
                    let a = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    let b = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_solve(a, b)))
                }
                FID_RANK => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_int(super::la_rank(s)))
                }
                FID_EIGVALS => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_eigvals(s)))
                }
                FID_TRACE => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_real(super::la_trace(s)))
                }
                FID_NORM => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    let kind = match arg_kind_default_fro(&args, 1) {
                        Some(k) => k,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_real(super::la_norm(s, kind)))
                }
                FID_SHAPE => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(opt_text(super::la_shape(s)))
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "linalg {}; nalgebra 0.33",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("linalg: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
