//! ID generators: ULID + nanoid + snowflake.

extern crate alloc;

use alloc::string::{String, ToString};
use core::sync::atomic::{AtomicI64, Ordering};

pub fn make_ulid() -> String {
    ulid::Ulid::new().to_string()
}

pub fn ulid_to_timestamp(s: &str) -> Result<i64, String> {
    let u = s
        .parse::<ulid::Ulid>()
        .map_err(|e| alloc::format!("ulid: parse: {e}"))?;
    Ok(u.timestamp_ms() as i64)
}

/// True iff `s` is a valid Crockford-base32 ULID (26 chars).
pub fn ulid_validate(s: &str) -> bool {
    s.parse::<ulid::Ulid>().is_ok()
}

/// Construct a ULID from an explicit ms-since-epoch + randomness.
/// `randomness` is the lower 80 bits taken from an unsigned BLOB-
/// shaped hex input. Useful for deterministic reproducible IDs in
/// tests; production should use `ulid()` for fresh randomness.
pub fn ulid_from_parts(timestamp_ms: u64, randomness_lo: u64, randomness_hi: u16) -> String {
    let rand = ((randomness_hi as u128) << 64) | (randomness_lo as u128);
    ulid::Ulid::from_parts(timestamp_ms, rand).to_string()
}

pub fn make_nanoid(size: usize) -> String {
    // The `nanoid!` macro takes a literal-only size arg; use
    // the function form for runtime size.
    let size = size.clamp(1, 256);
    nanoid::nanoid!(
        size,
        &[
            '_', '-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C',
            'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R',
            'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'a', 'b', 'c', 'd', 'e', 'f', 'g',
            'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v',
            'w', 'x', 'y', 'z'
        ]
    )
}

pub fn make_nanoid_custom(alphabet: &str, size: usize) -> Result<String, String> {
    let chars: alloc::vec::Vec<char> = alphabet.chars().collect();
    if chars.is_empty() {
        return Err("nanoid_custom: alphabet must be non-empty".to_string());
    }
    let size = size.clamp(1, 256);
    let mut out = String::with_capacity(size);
    // getrandom-backed uniform sampling. The bias is bounded
    // when the alphabet size is a power-of-2; for non-power-
    // of-2 alphabets the bias is < 1/256 (we sample u8 and
    // mod), which is the same as nanoid's default behaviour.
    let mut buf = alloc::vec![0u8; size];
    getrandom::getrandom(&mut buf).map_err(|e| alloc::format!("nanoid_custom: rng: {e}"))?;
    for &b in &buf {
        let idx = (b as usize) % chars.len();
        out.push(chars[idx]);
    }
    Ok(out)
}

// Snowflake: 41-bit timestamp (ms since custom epoch) +
// 10-bit worker + 12-bit sequence. Default epoch: Twitter's
// (2010-11-04 01:42:54.657 UTC = 1288834974657 ms).
const SNOWFLAKE_EPOCH_MS: i64 = 1288834974657;
const WORKER_BITS: i64 = 10;
const SEQ_BITS: i64 = 12;
const MAX_SEQ: i64 = (1 << SEQ_BITS) - 1;
const TS_SHIFT: i64 = WORKER_BITS + SEQ_BITS;
const WORKER_SHIFT: i64 = SEQ_BITS;

static SNOWFLAKE_SEQ: AtomicI64 = AtomicI64::new(0);
static SNOWFLAKE_LAST_TS: AtomicI64 = AtomicI64::new(0);

pub fn make_snowflake(worker_id: i64) -> Result<i64, String> {
    let worker = worker_id & ((1 << WORKER_BITS) - 1);
    // wasi-p2 doesn't have monotonic-ish std::time on every
    // build path; use a coarse millisecond clock via
    // std::time::SystemTime.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .map_err(|e| alloc::format!("snowflake: clock: {e}"))?;
    let ts = now_ms - SNOWFLAKE_EPOCH_MS;
    if ts < 0 {
        return Err("snowflake: clock pre-dates twitter epoch".to_string());
    }
    let last = SNOWFLAKE_LAST_TS.load(Ordering::Relaxed);
    let seq = if ts == last {
        let s = (SNOWFLAKE_SEQ.fetch_add(1, Ordering::Relaxed) + 1) & MAX_SEQ;
        if s == 0 {
            // Sequence wrapped within the same ms; spin until
            // the next ms. Rare under normal load (4096
            // generations / ms on one worker).
            loop {
                let again = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(now_ms);
                if again - SNOWFLAKE_EPOCH_MS > ts {
                    SNOWFLAKE_LAST_TS.store(again - SNOWFLAKE_EPOCH_MS, Ordering::Relaxed);
                    SNOWFLAKE_SEQ.store(0, Ordering::Relaxed);
                    return Ok(((again - SNOWFLAKE_EPOCH_MS) << TS_SHIFT)
                        | (worker << WORKER_SHIFT));
                }
            }
        }
        s
    } else {
        SNOWFLAKE_LAST_TS.store(ts, Ordering::Relaxed);
        SNOWFLAKE_SEQ.store(0, Ordering::Relaxed);
        0
    };
    Ok((ts << TS_SHIFT) | (worker << WORKER_SHIFT) | seq)
}

pub fn snowflake_to_timestamp(id: i64) -> i64 {
    (id >> TS_SHIFT) + SNOWFLAKE_EPOCH_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_round_trip() {
        let u = make_ulid();
        assert_eq!(u.len(), 26); // Crockford base32 of 128 bits
        let ts = ulid_to_timestamp(&u).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!((now - ts).abs() < 5_000);
    }

    #[test]
    fn nanoid_default_size() {
        assert_eq!(make_nanoid(21).len(), 21);
        assert_eq!(make_nanoid(8).len(), 8);
    }

    #[test]
    fn nanoid_custom_alphabet() {
        let n = make_nanoid_custom("ABC", 10).unwrap();
        assert_eq!(n.len(), 10);
        for c in n.chars() {
            assert!("ABC".contains(c));
        }
    }

    #[test]
    fn snowflake_round_trip() {
        let id = make_snowflake(7).unwrap();
        let ts = snowflake_to_timestamp(id);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!((now - ts).abs() < 5_000);
        // Two consecutive IDs differ.
        let id2 = make_snowflake(7).unwrap();
        assert_ne!(id, id2);
    }
}

#[cfg(feature = "embed")]
pub mod embed;

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

    const FID_ULID: u64 = 1;
    const FID_ULID_TS: u64 = 2;
    const FID_NANOID_0: u64 = 3;
    const FID_NANOID_1: u64 = 4;
    const FID_NANOID_CUSTOM: u64 = 5;
    const FID_SNOWFLAKE_0: u64 = 6;
    const FID_SNOWFLAKE_1: u64 = 7;
    const FID_SNOWFLAKE_TS: u64 = 8;
    const FID_VERSION: u64 = 9;
    const FID_ULID_VALIDATE: u64 = 10;
    const FID_ULID_FROM_PARTS: u64 = 11;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let nd = FunctionFlags::empty();
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: f,
            };
            Manifest {
                name: "ids".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ULID, "ulid", 0, nd),
                    s(FID_ULID_TS, "ulid_to_timestamp", 1, det),
                    s(FID_NANOID_0, "nanoid", 0, nd),
                    s(FID_NANOID_1, "nanoid", 1, nd),
                    s(FID_NANOID_CUSTOM, "nanoid_custom", 2, nd),
                    s(FID_SNOWFLAKE_0, "snowflake", 0, nd),
                    s(FID_SNOWFLAKE_1, "snowflake", 1, nd),
                    s(FID_SNOWFLAKE_TS, "snowflake_to_timestamp", 1, det),
                    s(FID_VERSION, "ids_version", 0, nd),
                    s(FID_ULID_VALIDATE, "ulid_validate", 1, det),
                    s(FID_ULID_FROM_PARTS, "ulid_from_parts", 3, det),
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

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(r)) => Ok(*r as i64),
            _ => Err(format!("{fname}: integer arg at {i}")),
        }
    }
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_ULID => Ok(SqlValue::Text(super::make_ulid())),
                FID_ULID_TS => {
                    let s = arg_text(&args, 0, "ulid_to_timestamp")?;
                    super::ulid_to_timestamp(&s).map(SqlValue::Integer)
                }
                FID_NANOID_0 => Ok(SqlValue::Text(super::make_nanoid(21))),
                FID_NANOID_1 => {
                    let n = arg_int(&args, 0, "nanoid")? as usize;
                    Ok(SqlValue::Text(super::make_nanoid(n)))
                }
                FID_NANOID_CUSTOM => {
                    let a = arg_text(&args, 0, "nanoid_custom")?;
                    let n = arg_int(&args, 1, "nanoid_custom")? as usize;
                    super::make_nanoid_custom(&a, n).map(SqlValue::Text)
                }
                FID_SNOWFLAKE_0 => super::make_snowflake(0).map(SqlValue::Integer),
                FID_SNOWFLAKE_1 => {
                    let w = arg_int(&args, 0, "snowflake")?;
                    super::make_snowflake(w).map(SqlValue::Integer)
                }
                FID_SNOWFLAKE_TS => {
                    let id = arg_int(&args, 0, "snowflake_to_timestamp")?;
                    Ok(SqlValue::Integer(super::snowflake_to_timestamp(id)))
                }
                FID_ULID_VALIDATE => {
                    let s = arg_text(&args, 0, "ulid_validate")?;
                    Ok(SqlValue::Integer(super::ulid_validate(&s) as i64))
                }
                FID_ULID_FROM_PARTS => {
                    let ts = arg_int(&args, 0, "ulid_from_parts")? as u64;
                    let lo = arg_int(&args, 1, "ulid_from_parts")? as u64;
                    let hi = arg_int(&args, 2, "ulid_from_parts")? as u16;
                    Ok(SqlValue::Text(super::ulid_from_parts(ts, lo, hi)))
                }
                other => Err(format!("ids: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
