//! LRU cache exposed as a bag of scalar SQL functions.
//!
//! Why scalars (not a vtab):
//!   A vtab needs persistent rowid identity + a destructor hooked
//!   into the connection lifecycle, which is awkward in the
//!   sqlite-wasm extension world (no per-connection guest state
//!   handle). A thread-local `LruCache` reaches the same usable
//!   shape — one cache per host VM instance — with a fraction of
//!   the surface area. This is the shape the brief calls for in
//!   `provenance/extensions.db` row 31.
//!
//! Lifetime + scope:
//!   The cache lives in the wasm guest's thread-local storage. The
//!   host instantiates one guest per loader scope, so for the
//!   `sqlite-wasm-run` CLI that means one cache per process. The
//!   cache is *not* persisted: values evaporate when the process
//!   exits. Two CLIs hitting the same .db file get two independent
//!   caches.
//!
//! Default capacity: 128 entries. `lru_capacity_set(n)` resizes
//! (truncating LRU-first if shrinking). `n` must be >= 1.
//!
//! Value typing:
//!   `lru_put` accepts INTEGER, REAL, TEXT, or BLOB. `lru_get`
//!   returns the same `SqlValue` variant that was stored (no type
//!   coercion). A `NULL` value is rejected — storing NULL would
//!   collide with the "key absent" return path of `lru_get`.
//!
//! Key typing:
//!   Keys are TEXT. Caller can hash/serialise whatever they need
//!   into a string. Storing raw blob keys would force a `Vec<u8>`
//!   `LruCache` and bloat the binary for no real win — the brief
//!   doesn't ask for it.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use core::num::NonZeroUsize;

    use lru::LruCache;

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
    const FID_PUT: u64 = 1;
    const FID_GET: u64 = 2;
    const FID_REMOVE: u64 = 3;
    const FID_CAPACITY: u64 = 4;
    const FID_CAPACITY_SET: u64 = 5;
    const FID_SIZE: u64 = 6;
    const FID_CLEAR: u64 = 7;
    const FID_VERSION: u64 = 8;

    // 128 is the conventional default for a small in-memory cache.
    // Big enough to be useful for materialising expensive computed
    // columns (the brief's example use case), small enough that a
    // fresh process doesn't blow memory before the user even sets
    // an explicit capacity.
    const DEFAULT_CAPACITY: usize = 128;

    /// Value variants we are willing to round-trip. NULL is
    /// deliberately excluded — see the module doc.
    #[derive(Clone)]
    enum StoredValue {
        Integer(i64),
        Real(f64),
        Text(String),
        Blob(Vec<u8>),
    }

    impl StoredValue {
        fn into_sql(self) -> SqlValue {
            match self {
                StoredValue::Integer(n) => SqlValue::Integer(n),
                StoredValue::Real(r) => SqlValue::Real(r),
                StoredValue::Text(s) => SqlValue::Text(s),
                StoredValue::Blob(b) => SqlValue::Blob(b),
            }
        }
    }

    thread_local! {
        // Default capacity is unwrap-safe: 128 is non-zero.
        static CACHE: RefCell<LruCache<String, StoredValue>> = RefCell::new(
            LruCache::new(NonZeroUsize::new(DEFAULT_CAPACITY).unwrap())
        );
    }

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            Some(SqlValue::Null) => Err(format!("{fname}: arg {i} (key) must be TEXT, got NULL")),
            _ => Err(format!("{fname}: arg {i} (key) must be TEXT")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: arg {i} must be INTEGER")),
        }
    }

    fn arg_storable(args: &[SqlValue], i: usize, fname: &str) -> Result<StoredValue, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(StoredValue::Integer(*n)),
            Some(SqlValue::Real(r)) => Ok(StoredValue::Real(*r)),
            Some(SqlValue::Text(s)) => Ok(StoredValue::Text(s.clone())),
            Some(SqlValue::Blob(b)) => Ok(StoredValue::Blob(b.clone())),
            Some(SqlValue::Null) => Err(format!(
                "{fname}: arg {i} (value) cannot be NULL; use lru_remove() to evict a key"
            )),
            None => Err(format!("{fname}: missing arg {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // All eight functions mutate or read shared state; none
            // are safe to mark DETERMINISTIC. SQLite would otherwise
            // be entitled to constant-fold a `lru_get('x')` across
            // multiple rows in a SELECT and miss intervening puts.
            // The lone exception is `lru_version`, a compile-time
            // constant readback.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "lru-cache".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_PUT, "lru_put", 2, nd),
                    s(FID_GET, "lru_get", 1, nd),
                    s(FID_REMOVE, "lru_remove", 1, nd),
                    s(FID_CAPACITY, "lru_capacity", 0, nd),
                    s(FID_CAPACITY_SET, "lru_capacity_set", 1, nd),
                    s(FID_SIZE, "lru_size", 0, nd),
                    s(FID_CLEAR, "lru_clear", 0, nd),
                    s(FID_VERSION, "lru_version", 0, FunctionFlags::DETERMINISTIC),
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
                FID_PUT => {
                    let key = arg_text(&args, 0, "lru_put")?;
                    let val = arg_storable(&args, 1, "lru_put")?;
                    let was_new = CACHE.with(|c| {
                        let mut c = c.borrow_mut();
                        // `put` returns Some(old) when overwriting,
                        // None when inserting fresh — flip that to
                        // 1=inserted / 0=updated so the SQL caller
                        // can branch on the truthier value.
                        c.put(key, val).is_none()
                    });
                    Ok(SqlValue::Integer(if was_new { 1 } else { 0 }))
                }
                FID_GET => {
                    let key = arg_text(&args, 0, "lru_get")?;
                    let found = CACHE.with(|c| {
                        let mut c = c.borrow_mut();
                        // `get` (mut) bumps the entry to MRU; that's
                        // the whole point of an LRU read.
                        c.get(&key).cloned()
                    });
                    match found {
                        Some(v) => Ok(v.into_sql()),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_REMOVE => {
                    let key = arg_text(&args, 0, "lru_remove")?;
                    let removed = CACHE.with(|c| c.borrow_mut().pop(&key).is_some());
                    Ok(SqlValue::Integer(if removed { 1 } else { 0 }))
                }
                FID_CAPACITY => {
                    let cap = CACHE.with(|c| c.borrow().cap().get() as i64);
                    Ok(SqlValue::Integer(cap))
                }
                FID_CAPACITY_SET => {
                    let cap = arg_int(&args, 0, "lru_capacity_set")?;
                    if cap < 1 {
                        return Err(format!(
                            "lru_capacity_set: capacity must be >= 1 (got {cap})"
                        ));
                    }
                    // usize fits any value SQLite can hand us on a
                    // 32-bit wasm target so long as cap > 0;
                    // NonZeroUsize::new only returns None on 0,
                    // which we've already rejected above.
                    let nz = NonZeroUsize::new(cap as usize).ok_or_else(|| {
                        "lru_capacity_set: capacity overflowed usize".to_string()
                    })?;
                    CACHE.with(|c| c.borrow_mut().resize(nz));
                    Ok(SqlValue::Integer(cap))
                }
                FID_SIZE => {
                    let n = CACHE.with(|c| c.borrow().len() as i64);
                    Ok(SqlValue::Integer(n))
                }
                FID_CLEAR => {
                    let cleared = CACHE.with(|c| {
                        let mut c = c.borrow_mut();
                        let n = c.len() as i64;
                        c.clear();
                        n
                    });
                    Ok(SqlValue::Integer(cleared))
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("lru-cache: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
