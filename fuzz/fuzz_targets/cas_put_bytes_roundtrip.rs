#![no_main]
//! Fuzz `SqliteCasStore::put` + `get_by_sha256` round-trip.
//! Properties:
//!   1. put(B) never panics for arbitrary B (including empty).
//!   2. put(B) is idempotent — calling twice returns the same hash.
//!   3. get_by_sha256(sha256(B)) returns the exact bytes back.
//!
//! Catches sha256-mirror bugs (cas-cache v2 schema added the mirror;
//! v3 bundles depend on it for content-hash lookups).
//!
//! Per-iter cost: tempfile + SQLite init. Slower than a pure-fn
//! target — keep input sizes small (libfuzzer caps default ~4KB).

use libfuzzer_sys::fuzz_target;
use sha2::{Digest, Sha256};
use sqlite_cas_cache::SqliteCasStore;

fuzz_target!(|bytes: &[u8]| {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = SqliteCasStore::open_external(dir.path().join("cas.sqlite"))
        .expect("open cas");

    let hash1 = store.put(bytes).expect("put");
    let hash2 = store.put(bytes).expect("put again");
    assert_eq!(hash1, hash2, "put not idempotent on bytes len {}", bytes.len());

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let sha: [u8; 32] = hasher.finalize().into();

    let got = store.get_by_sha256(&sha).expect("get_by_sha256");
    assert_eq!(got.as_deref(), Some(bytes),
        "round-trip mismatch for {} bytes", bytes.len());
});
