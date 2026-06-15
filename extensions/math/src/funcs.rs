//! Pure math implementations. Native-testable; the wit-bindgen
//! Guest impl in `lib.rs` is a thin dispatch wrapper that
//! materializes `Arg` values and routes by function id.

use alloc::string::{String, ToString};

#[derive(Debug, Clone)]
pub enum Arg {
    Null,
    Integer(i64),
    Real(f64),
}

/// Coerce an `Arg` to f64. Integers cast lossy at the edges of
/// i64's range, mirroring SQLite's own behavior.
pub fn to_f64(a: &Arg) -> Result<f64, String> {
    match a {
        Arg::Null => Err("NULL value".to_string()),
        Arg::Integer(i) => Ok(*i as f64),
        Arg::Real(r) => Ok(*r),
    }
}

/// Sign convention from SQLite's math1.c: 1 for positive, -1
/// for negative, 0 for zero.
pub fn sign(x: f64) -> i64 {
    if x > 0.0 {
        1
    } else if x < 0.0 {
        -1
    } else {
        0
    }
}

pub fn degrees(x: f64) -> f64 {
    x * 180.0 / core::f64::consts::PI
}

pub fn radians(x: f64) -> f64 {
    x * core::f64::consts::PI / 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn sign_cases() {
        assert_eq!(sign(2.5), 1);
        assert_eq!(sign(-0.5), -1);
        assert_eq!(sign(0.0), 0);
    }

    #[test]
    fn degree_radian_round_trip() {
        let r = radians(45.0);
        assert!(approx(degrees(r), 45.0, 1e-9));
    }

    #[test]
    fn radians_pi() {
        assert!(approx(radians(180.0), core::f64::consts::PI, 1e-9));
    }

    #[test]
    fn to_f64_round_trip() {
        assert_eq!(to_f64(&Arg::Integer(42)).unwrap(), 42.0);
        assert_eq!(to_f64(&Arg::Real(3.14)).unwrap(), 3.14);
        assert!(to_f64(&Arg::Null).is_err());
    }
}
