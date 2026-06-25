//! Tests for the resolver subsystem + fetch_artifact path.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use sqlite_cas_cache::{
    ArtifactRef, ArtifactResolver, LocalFileResolver, ResolverRegistry, Source, SqliteCasStore,
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
    let aref = ArtifactRef::from_source(Source::LocalFile { path }).with_expected_hash(wrong);
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

// Mutant guard: fetch_artifact's match `Some(expected) if expected != h`
// (store.rs:588) must NOT return the URI-bound artifact when its hash
// differs from the caller's expected_hash. Replacing `!=` with `==` (or
// the guard with `true`/`false`) lets the bound-URI bytes pass through
// even when they're the wrong artifact.
#[test]
fn fetch_artifact_rejects_uri_when_hash_mismatch() {
    let (_d, mut store) = fresh_store();
    // Pre-bind URI "u" to artifact A. Then ask for B via the same URI
    // with expected_hash=B. The bound A must not be returned.
    let hash_a = store.put(b"artifact-aaaa").unwrap();
    store.set_uri("u", &hash_a).unwrap();
    let hash_b = blake3::hash(b"artifact-bbbb");
    assert_ne!(hash_a, hash_b);
    // No source registered; fetch must fail rather than return A.
    let aref = ArtifactRef::from_source(Source::Blake3 { hash: hash_b })
        .with_uri("u")
        .with_expected_hash(hash_b);
    let registry = ResolverRegistry::new();
    let res = store.fetch_artifact(&aref, &registry);
    assert!(
        res.is_err(),
        "fetch_artifact returned URI-bound A despite expecting B"
    );
}

#[test]
fn fetch_artifact_binds_uri_on_success() {
    let (dir, mut store) = fresh_store();
    let payload = b"binds-uri";
    let path = dir.path().join("bound.bin");
    std::fs::write(&path, payload).unwrap();
    let aref = ArtifactRef::from_source(Source::LocalFile { path }).with_uri("local:bound");
    let registry = ResolverRegistry::with_builtins();
    let (hash, _) = store.fetch_artifact(&aref, &registry).unwrap();
    let (resolved_hash, resolved_bytes) = store.resolve_uri("local:bound").unwrap().unwrap();
    assert_eq!(resolved_hash, hash);
    assert_eq!(resolved_bytes, payload);
}

// ---------------------------------------------------------------------------
// HTTPS resolver fixtures via mockito (closes mutants 179 + 187).
//
// The HttpsResolver in src/resolver.rs:179 is unit-tested only against
// happy-path local mocks; the real 200/404/500/invalid-url branches
// were uncovered before this batch. Mockito spawns a localhost HTTP
// server per test, the resolver's `client.get(url)` hits it, and each
// branch lights up.
// ---------------------------------------------------------------------------

#[cfg(feature = "https")]
mod https_mock {
    use sqlite_cas_cache::{ArtifactResolver, Source};

    fn https_resolver() -> sqlite_cas_cache::HttpsResolver {
        sqlite_cas_cache::HttpsResolver::default()
    }

    #[test]
    fn https_resolves_200_returns_body() {
        let mut server = mockito::Server::new();
        let payload = b"hello-from-mock";
        let m = server
            .mock("GET", "/artifact.wasm")
            .with_status(200)
            .with_body(payload)
            .create();
        let url = format!("{}/artifact.wasm", server.url());
        let r = https_resolver();
        let bytes = r
            .resolve(&Source::Https { url })
            .expect("200 should resolve to body");
        assert_eq!(bytes, payload);
        m.assert();
    }

    #[test]
    fn https_404_returns_error() {
        let mut server = mockito::Server::new();
        let m = server.mock("GET", "/missing").with_status(404).create();
        let url = format!("{}/missing", server.url());
        let r = https_resolver();
        let err = r
            .resolve(&Source::Https { url })
            .expect_err("404 should propagate as error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("404"),
            "404 error message should reference the status, got: {msg}"
        );
        m.assert();
    }

    #[test]
    fn https_500_returns_error() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("GET", "/server-err")
            .with_status(500)
            .with_body("oops")
            .create();
        let url = format!("{}/server-err", server.url());
        let r = https_resolver();
        let err = r
            .resolve(&Source::Https { url })
            .expect_err("500 should propagate as error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("500"),
            "500 error message should reference the status, got: {msg}"
        );
        m.assert();
    }

    #[test]
    fn https_wrong_source_kind_returns_error() {
        let r = https_resolver();
        let err = r
            .resolve(&Source::LocalFile { path: "/tmp/x".into() })
            .expect_err("LocalFile source on HTTPS resolver should error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("wrong source kind"),
            "wrong-kind path didn't match expected message, got: {msg}"
        );
    }

    #[test]
    fn https_invalid_url_returns_error() {
        // Malformed URL ("not-a-url") -> reqwest::Client::get refuses to send.
        // We don't need mockito here; reqwest's URL parser rejects the request
        // before it leaves the client. The error path is the same one mutants
        // 179/187 ask about: an Err propagates out of `resolve`, not a panic.
        let r = https_resolver();
        let err = r
            .resolve(&Source::Https {
                url: "not-a-url".to_string(),
            })
            .expect_err("malformed URL should error");
        let _msg = format!("{err:#}");
    }

    // Note on timeout coverage: HttpsResolver currently uses a default
    // reqwest::blocking::Client with no per-call timeout configured. A
    // timeout test would either (a) sleep > the platform's default
    // connect timeout (flaky in CI) or (b) require a resolver-side
    // timeout knob that doesn't exist yet. Deferred + tracked under
    // PLAN-followups.md.
}
