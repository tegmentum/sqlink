//! port of SQLite zorder.c (Morton curve)

extern crate alloc;

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

    // Arity-overloaded zorder() at 2, 3, 4, and 5 dimensions.
    const FID_ZORDER_2: u64 = 1;
    const FID_ZORDER_3: u64 = 2;
    const FID_ZORDER_4: u64 = 3;
    const FID_ZORDER_5: u64 = 4;
    const FID_UNZORDER: u64 = 5;

    struct Ext;

    /// Interleave the low bits of `coords` into a single u64 Z-order
    /// (Morton) index. Output bit b of position p comes from input
    /// coord `p % N`, bit `p / N`. Caller is responsible for keeping
    /// coords small enough that the total bits fit (64 / N per coord).
    /// Reference: ext/misc/zorder.c (interleaving bits).
    fn zorder(coords: &[i64]) -> i64 {
        let n = coords.len();
        if n == 0 || n > 64 {
            return 0;
        }
        let mut out: u64 = 0;
        let mut shifted: Vec<u64> = coords.iter().map(|&c| c as u64).collect();
        // Walk bit by bit from LSB. At step b, take bit 0 of each
        // shifted coord and place at position b*N+i.
        let bits_per_coord = 64 / n as u32;
        for b in 0..bits_per_coord {
            for (i, c) in shifted.iter_mut().enumerate() {
                if *c & 1 != 0 {
                    out |= 1u64 << (b * n as u32 + i as u32);
                }
                *c >>= 1;
            }
        }
        out as i64
    }

    /// Extract dimension `i` from an N-dimensional Z-order index `z`.
    /// `i` is 0-indexed.
    fn unzorder(z: i64, n: u32, i: u32) -> Option<i64> {
        if n == 0 || n > 64 || i >= n {
            return None;
        }
        let bits_per_coord = 64 / n;
        let mut out: u64 = 0;
        let zu = z as u64;
        for b in 0..bits_per_coord {
            if zu & (1u64 << (b * n + i)) != 0 {
                out |= 1u64 << b;
            }
        }
        Some(out as i64)
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
                name: "zorder".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // Arity-overloaded zorder() matches the SQLite
                    // zorder.c surface; one entry per supported arity.
                    s(FID_ZORDER_2, "zorder", 2, det),
                    s(FID_ZORDER_3, "zorder", 3, det),
                    s(FID_ZORDER_4, "zorder", 4, det),
                    s(FID_ZORDER_5, "zorder", 5, det),
                    s(FID_UNZORDER, "unzorder", 3, det),
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
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_UNZORDER => {
                    let z = arg_int(&args, 0, "unzorder")?;
                    let n = arg_int(&args, 1, "unzorder")? as u32;
                    let i = arg_int(&args, 2, "unzorder")? as u32;
                    Ok(unzorder(z, n, i)
                        .map(SqlValue::Integer)
                        .unwrap_or(SqlValue::Null))
                }
                FID_ZORDER_2 | FID_ZORDER_3 | FID_ZORDER_4 | FID_ZORDER_5 => {
                    let mut coords: Vec<i64> = alloc::vec![];
                    for (i, _) in args.iter().enumerate() {
                        coords.push(arg_int(&args, i, "zorder")?);
                    }
                    Ok(SqlValue::Integer(zorder(&coords)))
                }
                other => Err(format!("zorder: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
