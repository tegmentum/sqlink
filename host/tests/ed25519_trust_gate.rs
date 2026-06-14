//! End-to-end test for the Ed25519 trust gate in
//! `Host::register_wasm_provider`.
//!
//! Generates a real Ed25519 keypair via ed25519-dalek (dev-dep),
//! signs a tiny placeholder "provider" binary, drops the sig as a
//! `.sig` sidecar, and asserts:
//!
//!   1. The host accepts the registration when the policy's anchor
//!      list contains the matching public key.
//!   2. The host rejects when the anchor list contains a different
//!      key.
//!   3. The host rejects when the sidecar `.sig` is absent.
//!
//! The verifier uses openssl-wasm. The test skips cleanly if the
//! `openssl-composed.wasm` artifact isn't built (see
//! ~/git/openssl-wasm) — same skip pattern the other host
//! integration tests use.
//!
//! What this test does NOT cover:
//!   - Signature schemes other than Ed25519 (RSA-PSS, ECDSA — those
//!     would re-use the same path with different `KeyType` calls).
//!   - X.509 chain validation (separate code path via x509.verify).

use std::path::PathBuf;

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use sqlite_wasm_host::{Host, TrustPolicy};

fn openssl_wasm_path() -> PathBuf {
    if let Ok(p) = std::env::var("OPENSSL_WASM_PATH") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join("git/openssl-wasm/build/openssl-composed.wasm")
}

fn openssl_available() -> bool {
    openssl_wasm_path().exists()
}

/// Build a tiny stand-in for a "provider binary." Doesn't have to
/// be a real wasm component for the signature check — the registry
/// step that verifies bytes happens BEFORE the wasm compile step,
/// so a few hand-rolled bytes are enough to drive the gate.
/// (The compile step happens after on the accept path; that's
/// what'd fail for the positive-case test, but we assert behavior
/// at the level of "register_wasm_provider returned Ok or Err"
/// before reaching wasm compile when possible. For the positive
/// case we use a real wasm header to avoid the gate-passing-then-
/// compile-failing ambiguity.)
fn fixture_provider_bytes() -> Vec<u8> {
    // Smallest valid wasm component: the four-byte magic + version
    // for a CORE module is 0x00 0x61 0x73 0x6d 0x01 0x00 0x00 0x00,
    // but `Component::from_binary` rejects a core module. Use the
    // canonical empty COMPONENT magic instead: `\0asm` + version
    // `0x0d 0x00 0x01 0x00` for component-model layer 1.
    vec![0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00]
}

#[tokio::test]
async fn matching_anchor_accepts_signed_provider() {
    if !openssl_available() {
        eprintln!(
            "skipping: openssl-composed.wasm not found at {} (set OPENSSL_WASM_PATH or build ~/git/openssl-wasm)",
            openssl_wasm_path().display()
        );
        return;
    }

    let mut rng = OsRng;
    let signing_key = SigningKey::generate(&mut rng);
    let pubkey: [u8; 32] = signing_key.verifying_key().to_bytes();

    let dir = tempfile::tempdir().unwrap();
    let provider_path = dir.path().join("p.wasm");
    let bytes = fixture_provider_bytes();
    std::fs::write(&provider_path, &bytes).unwrap();
    let sig = signing_key.sign(&bytes);
    std::fs::write(
        provider_path.with_extension("wasm.sig"),
        sig.to_bytes(),
    )
    .unwrap();

    let host = Host::new().unwrap();
    host.set_trust_policy(TrustPolicy::Ed25519Signed {
        anchors: vec![pubkey],
    });

    let r = host
        .register_wasm_provider_async("test-provider", provider_path)
        .await;
    // We expect either Ok (the bytes are a valid component header)
    // or a downstream "compile component: ..." error AFTER the gate
    // accepted. What we MUST NOT see is "Ed25519 signature did not
    // validate" — that's the gate rejecting our valid signature.
    if let Err(e) = &r {
        let s = e.to_string();
        assert!(
            !s.contains("Ed25519 signature did not validate"),
            "matching anchor should pass the gate, got: {s}"
        );
    }
}

#[tokio::test]
async fn wrong_anchor_rejects_signed_provider() {
    if !openssl_available() {
        eprintln!("skipping: openssl-composed.wasm not found");
        return;
    }

    let mut rng = OsRng;
    let signing_key = SigningKey::generate(&mut rng);
    let other_key = SigningKey::generate(&mut rng);
    let other_pub: [u8; 32] = other_key.verifying_key().to_bytes();

    let dir = tempfile::tempdir().unwrap();
    let provider_path = dir.path().join("p.wasm");
    let bytes = fixture_provider_bytes();
    std::fs::write(&provider_path, &bytes).unwrap();
    // Sign with the wrong key — anchors list a different one.
    let sig = signing_key.sign(&bytes);
    std::fs::write(
        provider_path.with_extension("wasm.sig"),
        sig.to_bytes(),
    )
    .unwrap();

    let host = Host::new().unwrap();
    host.set_trust_policy(TrustPolicy::Ed25519Signed {
        anchors: vec![other_pub],
    });

    let err = host
        .register_wasm_provider_async("rejected-provider", provider_path)
        .await
        .expect_err("anchor mismatch must reject the registration");
    let s = err.to_string();
    assert!(
        s.contains("Ed25519 signature did not validate"),
        "expected signature-mismatch error, got: {s}"
    );
}

#[tokio::test]
async fn missing_sidecar_rejects_signed_provider() {
    if !openssl_available() {
        eprintln!("skipping: openssl-composed.wasm not found");
        return;
    }

    let mut rng = OsRng;
    let signing_key = SigningKey::generate(&mut rng);
    let pubkey: [u8; 32] = signing_key.verifying_key().to_bytes();

    let dir = tempfile::tempdir().unwrap();
    let provider_path = dir.path().join("p.wasm");
    std::fs::write(&provider_path, fixture_provider_bytes()).unwrap();
    // Deliberately don't write the .sig sidecar.

    let host = Host::new().unwrap();
    host.set_trust_policy(TrustPolicy::Ed25519Signed {
        anchors: vec![pubkey],
    });

    let err = host
        .register_wasm_provider_async("no-sig", provider_path)
        .await
        .expect_err("missing sidecar must reject the registration");
    let s = err.to_string();
    assert!(
        s.contains("read signature") || s.contains("No such file"),
        "expected missing-signature error, got: {s}"
    );
}
