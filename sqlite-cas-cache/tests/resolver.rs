//! Tests for the resolver subsystem + fetch_artifact path.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use sqlite_cas_cache::{
    ArtifactRef, ArtifactResolver, LocalFileResolver, ResolverRegistry, Source,
    SqliteCasStore,
};

fn fresh_store() -> (tempfile::TempDir, SqliteCasStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteCasStore::open_external(dir.path().join("cas.sqlite")).unwrap();
    (dir, store)
}

#[test]
fn local_file_resolver_supports_local_kind() {
    let r = LocalFileResolver;
    assert!(r.supported_kinds().contains(&"local"));
}

#[test]
fn registry_with_builtins_finds_local() {
    let registry = ResolverRegistry::with_builtins();
    assert!(registry.find("local").is_some());
    assert!(registry.find("nonsense").is_none());
}

#[cfg(feature = "https")]
#[test]
fn registry_with_builtins_finds_https() {
    let registry = ResolverRegistry::with_builtins();
    assert!(registry.find("https").is_some());
}

#[test]
fn fetch_artifact_resolves_local_file_and_caches() {
    let (dir, mut store) = fresh_store();
    let payload = b"local-file-payload";
    let path = dir.path().join("artifact.wasm");
    std::fs::write(&path, payload).unwrap();
    let registry = ResolverRegistry::with_builtins();
    let aref = ArtifactRef::from_source(Source::LocalFile { path: path.clone() })
        .with_uri("file://artifact.wasm");
    let (hash, bytes) = store.fetch_artifact(&aref, &registry).unwrap();
    assert_eq!(bytes, payload);
    assert_eq!(hash, blake3::hash(payload));
    // URI was bound; second call short-circuits via resolve_uri.
    let (hit_hash, hit_bytes) = store
        .fetch_artifact(&aref, &ResolverRegistry::new())
        .unwrap();
    assert_eq!(hit_hash, hash);
    assert_eq!(hit_bytes, payload);
}

#[test]
fn fetch_artifact_blake3_hits_existing_store_entry() {
    let (_dir, mut store) = fresh_store();
    let bytes = b"already-stored";
    let put_hash = store.put(bytes).unwrap();
    let aref = ArtifactRef::from_source(Source::Blake3 { hash: put_hash });
    let registry = ResolverRegistry::new();
    let (got_hash, got_bytes) = store.fetch_artifact(&aref, &registry).unwrap();
    assert_eq!(got_hash, put_hash);
    assert_eq!(got_bytes, bytes);
}

#[test]
fn fetch_artifact_falls_through_failing_sources() {
    let (dir, mut store) = fresh_store();
    let payload = b"second-source-wins";
    let good = dir.path().join("good.bin");
    std::fs::write(&good, payload).unwrap();
    let missing: PathBuf = dir.path().join("does-not-exist.bin");
    let registry = ResolverRegistry::with_builtins();
    let aref = ArtifactRef::default()
        .add_source(Source::LocalFile { path: missing })
        .add_source(Source::LocalFile { path: good })
        .with_uri("file://second");
    let (_h, b) = store.fetch_artifact(&aref, &registry).unwrap();
    assert_eq!(b, payload);
}

#[test]
fn fetch_artifact_returns_last_error_when_all_sources_fail() {
    let (dir, mut store) = fresh_store();
    let missing_a = dir.path().join("nope-a.bin");
    let missing_b = dir.path().join("nope-b.bin");
    let aref = ArtifactRef::default()
        .add_source(Source::LocalFile { path: missing_a })
        .add_source(Source::LocalFile { path: missing_b });
    let err = store
        .fetch_artifact(&aref, &ResolverRegistry::with_builtins())
        .unwrap_err();
    // The last attempted source is the b-file; error chain
    // mentions reading it.
    assert!(format!("{err:?}").contains("nope-b.bin"), "{err:?}");
}

#[test]
fn fetch_artifact_rejects_hash_mismatch() {
    let (dir, mut store) = fresh_store();
    let payload = b"truthful-bytes";
    let path = dir.path().join("a.bin");
    std::fs::write(&path, payload).unwrap();
    let wrong = blake3::hash(b"different-bytes");
    let aref = ArtifactRef::from_source(Source::LocalFile { path })
        .with_expected_hash(wrong);
    let err = store
        .fetch_artifact(&aref, &ResolverRegistry::with_builtins())
        .unwrap_err();
    assert!(format!("{err:?}").contains("hash mismatch"), "{err:?}");
}

#[test]
fn fetch_artifact_uses_expected_hash_to_short_circuit_cache() {
    let (_dir, mut store) = fresh_store();
    let payload = b"already-here";
    let stored_hash = store.put(payload).unwrap();
    // No sources at all; only the expected_hash. Cache hit
    // should still succeed.
    let aref = ArtifactRef::default().with_expected_hash(stored_hash);
    let (h, b) = store
        .fetch_artifact(&aref, &ResolverRegistry::new())
        .unwrap();
    assert_eq!(h, stored_hash);
    assert_eq!(b, payload);
}

#[test]
fn fetch_artifact_no_resolver_for_kind_errs() {
    let (_dir, mut store) = fresh_store();
    let aref = ArtifactRef::from_source(Source::Custom {
        kind: "ipfs".into(),
        payload: vec![],
    });
    let err = store
        .fetch_artifact(&aref, &ResolverRegistry::new())
        .unwrap_err();
    assert!(format!("{err:?}").contains("ipfs"), "{err:?}");
}

#[test]
fn custom_resolver_dispatches_by_kind() {
    let (_dir, mut store) = fresh_store();

    struct ConstResolver(Vec<u8>);
    impl ArtifactResolver for ConstResolver {
        fn supported_kinds(&self) -> &[&str] {
            &["const"]
        }
        fn resolve(&self, source: &Source) -> Result<Vec<u8>> {
            match source {
                Source::Custom { kind, .. } if kind == "const" => Ok(self.0.clone()),
                _ => Err(anyhow!("not a const source")),
            }
        }
    }

    let payload = b"resolved-by-custom".to_vec();
    let mut registry = ResolverRegistry::new();
    registry.register(Arc::new(ConstResolver(payload.clone())));
    let aref = ArtifactRef::from_source(Source::Custom {
        kind: "const".into(),
        payload: vec![],
    })
    .with_uri("const://x");
    let (h, b) = store.fetch_artifact(&aref, &registry).unwrap();
    assert_eq!(b, payload);
    assert_eq!(h, blake3::hash(&payload));
}

#[test]
fn fetch_artifact_binds_uri_on_success() {
    let (dir, mut store) = fresh_store();
    let payload = b"binds-uri";
    let path = dir.path().join("bound.bin");
    std::fs::write(&path, payload).unwrap();
    let aref = ArtifactRef::from_source(Source::LocalFile { path })
        .with_uri("local:bound");
    let registry = ResolverRegistry::with_builtins();
    let (hash, _) = store.fetch_artifact(&aref, &registry).unwrap();
    let (resolved_hash, resolved_bytes) =
        store.resolve_uri("local:bound").unwrap().unwrap();
    assert_eq!(resolved_hash, hash);
    assert_eq!(resolved_bytes, payload);
}
