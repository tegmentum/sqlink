//! Single-CAS consolidation (Stage C): `SqliteCasStore` round-trips
//! blobs through the shared `compose_core::blobs::BlobBackend` trait,
//! addressed by sha-256. Only built with `--features compose`.
#![cfg(feature = "compose")]

use compose_core::blobs::{compute_digest, BlobBackend};
use compose_core::CompileCache;
use sqlite_cas_cache::SqliteCasStore;

fn store() -> SqliteCasStore {
    SqliteCasStore::open_external(":memory:").unwrap()
}

#[test]
fn put_get_by_sha256_roundtrips_through_trait() {
    let s = store();
    let backend: &dyn BlobBackend = &s;

    let data = b"single-cas sha256 round-trip";
    let digest = backend.put(data).unwrap();

    // The trait Digest is the framework sha-256.
    assert_eq!(digest, compute_digest(data));
    assert_eq!(digest.len(), 32);

    assert!(backend.has(&digest));
    assert_eq!(backend.size(&digest), Some(data.len() as u64));
    assert_eq!(backend.get(&digest).unwrap(), data);

    let all = backend.list_all();
    assert!(all.contains(&digest));
}

#[test]
fn missing_blob_is_not_found() {
    let s = store();
    let backend: &dyn BlobBackend = &s;
    let absent = compute_digest(b"never stored");
    assert!(!backend.has(&absent));
    assert!(backend.size(&absent).is_none());
    let err = backend.get(&absent).unwrap_err();
    assert_eq!(err.code, compose_core::types::ErrorCode::BlobNotFound);
}

#[test]
fn delete_removes_unbound_blob() {
    let s = store();
    let backend: &dyn BlobBackend = &s;
    let data = b"deletable";
    let digest = backend.put(data).unwrap();
    assert!(backend.has(&digest));
    backend.delete(&digest).unwrap();
    assert!(!backend.has(&digest));
    // Second delete -> not found.
    assert_eq!(
        backend.delete(&digest).unwrap_err().code,
        compose_core::types::ErrorCode::BlobNotFound
    );
}

#[test]
fn put_is_idempotent() {
    let s = store();
    let backend: &dyn BlobBackend = &s;
    let data = b"same bytes twice";
    let d1 = backend.put(data).unwrap();
    let d2 = backend.put(data).unwrap();
    assert_eq!(d1, d2);
    assert_eq!(backend.list_all().len(), 1);
}

#[test]
fn compile_cache_over_sqlite_backend_seals_and_opens() {
    // The store plugs straight into the shared trust-gated cache.
    let cache = CompileCache::new(store(), b"host-local-secret".to_vec());
    let component = compute_digest(b"a-component");
    let artifact = b"precompiled machine code";

    assert!(cache
        .load(&component, "engine-1", "aarch64-macos")
        .unwrap()
        .is_none());

    cache
        .store(&component, "engine-1", "aarch64-macos", artifact)
        .unwrap();
    let got = cache
        .load(&component, "engine-1", "aarch64-macos")
        .unwrap();
    assert_eq!(got.as_deref(), Some(&artifact[..]));

    // Wrong engine version -> miss.
    assert!(cache
        .load(&component, "engine-2", "aarch64-macos")
        .unwrap()
        .is_none());

    // seal/open frame round-trips and rejects tamper.
    let sealed = cache.seal(&component, "engine-1", "t", artifact);
    assert_eq!(
        cache.open(&component, "engine-1", "t", &sealed).as_deref(),
        Some(&artifact[..])
    );
    let mut bad = sealed.clone();
    *bad.last_mut().unwrap() ^= 0xFF;
    assert!(cache.open(&component, "engine-1", "t", &bad).is_none());
}
