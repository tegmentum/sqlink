//! Key-based crypto: ed25519, x25519, AEAD ciphers, merkle.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::String;
use alloc::vec::Vec;

// ── ed25519 ─────────────────────────────────────────────────

pub fn ed25519_keygen() -> Vec<u8> {
    use rand_core::OsRng;
    let mut bytes = [0u8; 32];
    use rand_core::RngCore;
    OsRng.fill_bytes(&mut bytes);
    bytes.to_vec()
}

pub fn ed25519_public(secret: &[u8]) -> Result<Vec<u8>, String> {
    use ed25519_dalek::SigningKey;
    if secret.len() != 32 {
        return Err("ed25519_public: secret must be 32 bytes".into());
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(secret);
    let sk = SigningKey::from_bytes(&seed);
    Ok(sk.verifying_key().to_bytes().to_vec())
}

pub fn ed25519_sign(secret: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    use ed25519_dalek::{Signer, SigningKey};
    if secret.len() != 32 {
        return Err("ed25519_sign: secret must be 32 bytes".into());
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(secret);
    let sk = SigningKey::from_bytes(&seed);
    Ok(sk.sign(msg).to_bytes().to_vec())
}

pub fn ed25519_verify(public: &[u8], sig: &[u8], msg: &[u8]) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    if public.len() != 32 || sig.len() != 64 {
        return false;
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(public);
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    let mut s = [0u8; 64];
    s.copy_from_slice(sig);
    let signature = Signature::from_bytes(&s);
    vk.verify(msg, &signature).is_ok()
}

// ── x25519 ──────────────────────────────────────────────────

pub fn x25519_keygen() -> Vec<u8> {
    use rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    // Standard x25519 clamping.
    bytes[0] &= 248;
    bytes[31] &= 127;
    bytes[31] |= 64;
    bytes.to_vec()
}

pub fn x25519_public(secret: &[u8]) -> Result<Vec<u8>, String> {
    if secret.len() != 32 {
        return Err("x25519_public: secret must be 32 bytes".into());
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(secret);
    let sk = x25519_dalek::StaticSecret::from(bytes);
    Ok(x25519_dalek::PublicKey::from(&sk).as_bytes().to_vec())
}

pub fn x25519_shared(mine: &[u8], theirs: &[u8]) -> Result<Vec<u8>, String> {
    if mine.len() != 32 || theirs.len() != 32 {
        return Err("x25519_shared: keys must be 32 bytes".into());
    }
    let mut m = [0u8; 32];
    m.copy_from_slice(mine);
    let mut t = [0u8; 32];
    t.copy_from_slice(theirs);
    let sk = x25519_dalek::StaticSecret::from(m);
    let pk = x25519_dalek::PublicKey::from(t);
    let shared = sk.diffie_hellman(&pk);
    Ok(shared.as_bytes().to_vec())
}

// ── AEAD ciphers ────────────────────────────────────────────

pub fn chacha20poly1305_encrypt(
    key: &[u8],
    nonce: &[u8],
    ad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::ChaCha20Poly1305;
    if key.len() != 32 {
        return Err("chacha20poly1305: key must be 32 bytes".into());
    }
    if nonce.len() != 12 {
        return Err("chacha20poly1305: nonce must be 12 bytes".into());
    }
    let cipher = ChaCha20Poly1305::new(key.into());
    cipher
        .encrypt(nonce.into(), Payload { msg: plaintext, aad: ad })
        .map_err(|e| alloc::format!("chacha20poly1305_encrypt: {e}"))
}

pub fn chacha20poly1305_decrypt(
    key: &[u8],
    nonce: &[u8],
    ad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::ChaCha20Poly1305;
    if key.len() != 32 || nonce.len() != 12 {
        return Err("chacha20poly1305: bad key/nonce size".into());
    }
    let cipher = ChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(nonce.into(), Payload { msg: ciphertext, aad: ad })
        .map_err(|e| alloc::format!("chacha20poly1305_decrypt: {e}"))
}

// ── merkle ──────────────────────────────────────────────────

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

/// `leaves_blob` is a concatenated stream of leaf hashes, each
/// `leaf_size` bytes long. Returns the binary merkle root over
/// SHA-256. Default leaf_size = 32 (SHA-256 hashes).
pub fn merkle_root(leaves_blob: &[u8], leaf_size: usize) -> Result<Vec<u8>, String> {
    if leaf_size == 0 || leaves_blob.len() % leaf_size != 0 {
        return Err("merkle: leaves_blob length not multiple of leaf_size".into());
    }
    if leaves_blob.is_empty() {
        return Ok(vec![0u8; 32]);
    }
    let mut layer: Vec<[u8; 32]> = leaves_blob
        .chunks_exact(leaf_size)
        .map(|chunk| {
            if leaf_size == 32 {
                let mut out = [0u8; 32];
                out.copy_from_slice(chunk);
                out
            } else {
                sha256(chunk)
            }
        })
        .collect();
    while layer.len() > 1 {
        let mut next: Vec<[u8; 32]> = Vec::with_capacity(layer.len().div_ceil(2));
        for pair in layer.chunks(2) {
            let h = if pair.len() == 2 {
                let mut concat = [0u8; 64];
                concat[..32].copy_from_slice(&pair[0]);
                concat[32..].copy_from_slice(&pair[1]);
                sha256(&concat)
            } else {
                // Odd leaf  duplicate (Bitcoin-style).
                let mut concat = [0u8; 64];
                concat[..32].copy_from_slice(&pair[0]);
                concat[32..].copy_from_slice(&pair[0]);
                sha256(&concat)
            };
            next.push(h);
        }
        layer = next;
    }
    Ok(layer[0].to_vec())
}

/// Verify a merkle proof. `proof_blob` is concatenated
/// (direction_byte, 32-byte sibling) pairs from leaf to root:
/// direction 0 = sibling on left, 1 = sibling on right.
pub fn merkle_proof_verify(root: &[u8], leaf: &[u8], proof_blob: &[u8]) -> bool {
    if root.len() != 32 || leaf.len() != 32 || proof_blob.len() % 33 != 0 {
        return false;
    }
    let mut cur = [0u8; 32];
    cur.copy_from_slice(leaf);
    for step in proof_blob.chunks_exact(33) {
        let dir = step[0];
        let sibling = &step[1..33];
        let mut concat = [0u8; 64];
        if dir == 0 {
            concat[..32].copy_from_slice(sibling);
            concat[32..].copy_from_slice(&cur);
        } else {
            concat[..32].copy_from_slice(&cur);
            concat[32..].copy_from_slice(sibling);
        }
        cur = sha256(&concat);
    }
    cur.as_slice() == root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ed25519_round_trip() {
        let sk = ed25519_keygen();
        let pk = ed25519_public(&sk).unwrap();
        let sig = ed25519_sign(&sk, b"hello").unwrap();
        assert!(ed25519_verify(&pk, &sig, b"hello"));
        assert!(!ed25519_verify(&pk, &sig, b"goodbye"));
    }

    #[test]
    fn x25519_shared_matches() {
        let a_sk = x25519_keygen();
        let b_sk = x25519_keygen();
        let a_pk = x25519_public(&a_sk).unwrap();
        let b_pk = x25519_public(&b_sk).unwrap();
        let s1 = x25519_shared(&a_sk, &b_pk).unwrap();
        let s2 = x25519_shared(&b_sk, &a_pk).unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn chacha20_round_trip() {
        let key = [0xAAu8; 32];
        let nonce = [0xBBu8; 12];
        let ad = b"context";
        let pt = b"secret message";
        let ct = chacha20poly1305_encrypt(&key, &nonce, ad, pt).unwrap();
        let back = chacha20poly1305_decrypt(&key, &nonce, ad, &ct).unwrap();
        assert_eq!(back, pt);
        // Tampered AD fails.
        assert!(chacha20poly1305_decrypt(&key, &nonce, b"wrong", &ct).is_err());
    }

    #[test]
    fn merkle_root_two_leaves() {
        let a = sha256(b"a");
        let b = sha256(b"b");
        let mut leaves = vec![];
        leaves.extend_from_slice(&a);
        leaves.extend_from_slice(&b);
        let root = merkle_root(&leaves, 32).unwrap();
        // Recompute manually.
        let mut concat = [0u8; 64];
        concat[..32].copy_from_slice(&a);
        concat[32..].copy_from_slice(&b);
        let expected = sha256(&concat);
        assert_eq!(root, expected);
    }

    #[test]
    fn merkle_proof_verifies() {
        let a = sha256(b"a");
        let b = sha256(b"b");
        let mut leaves = vec![];
        leaves.extend_from_slice(&a);
        leaves.extend_from_slice(&b);
        let root = merkle_root(&leaves, 32).unwrap();
        // Proof for `a`: sibling on right is `b`.
        let mut proof = vec![1u8]; // dir=1: sibling on right
        proof.extend_from_slice(&b);
        assert!(merkle_proof_verify(&root, &a, &proof));
        // Wrong leaf fails.
        assert!(!merkle_proof_verify(&root, &b, &proof));
    }
}

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

    const FID_ED_KEYGEN: u64 = 1;
    const FID_ED_PUBLIC: u64 = 2;
    const FID_ED_SIGN: u64 = 3;
    const FID_ED_VERIFY: u64 = 4;
    const FID_X_KEYGEN: u64 = 5;
    const FID_X_PUBLIC: u64 = 6;
    const FID_X_SHARED: u64 = 7;
    const FID_CHACHA_ENC: u64 = 8;
    const FID_CHACHA_DEC: u64 = 9;
    const FID_MERKLE_ROOT: u64 = 12;
    const FID_MERKLE_VERIFY: u64 = 13;

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
                name: "crypto-keys".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ED_KEYGEN, "ed25519_keygen", 0, nd),
                    s(FID_ED_PUBLIC, "ed25519_public", 1, det),
                    s(FID_ED_SIGN, "ed25519_sign", 2, det),
                    s(FID_ED_VERIFY, "ed25519_verify", 3, det),
                    s(FID_X_KEYGEN, "x25519_keygen", 0, nd),
                    s(FID_X_PUBLIC, "x25519_public", 1, det),
                    s(FID_X_SHARED, "x25519_shared", 2, det),
                    s(FID_CHACHA_ENC, "chacha20poly1305_encrypt", 4, det),
                    s(FID_CHACHA_DEC, "chacha20poly1305_decrypt", 4, det),
                    s(FID_MERKLE_ROOT, "merkle_root", 2, det),
                    s(FID_MERKLE_VERIFY, "merkle_proof_verify", 3, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    fn arg_blob<'a>(args: &'a [SqlValue], i: usize, fname: &str) -> Result<&'a [u8], String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(r)) => Ok(*r as i64),
            _ => Err(format!("{fname}: integer arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_ED_KEYGEN => Ok(SqlValue::Blob(super::ed25519_keygen())),
                FID_ED_PUBLIC => super::ed25519_public(arg_blob(&args, 0, "ed25519_public")?)
                    .map(SqlValue::Blob),
                FID_ED_SIGN => super::ed25519_sign(
                    arg_blob(&args, 0, "ed25519_sign")?,
                    arg_blob(&args, 1, "ed25519_sign")?,
                )
                .map(SqlValue::Blob),
                FID_ED_VERIFY => Ok(SqlValue::Integer(super::ed25519_verify(
                    arg_blob(&args, 0, "ed25519_verify")?,
                    arg_blob(&args, 1, "ed25519_verify")?,
                    arg_blob(&args, 2, "ed25519_verify")?,
                ) as i64)),
                FID_X_KEYGEN => Ok(SqlValue::Blob(super::x25519_keygen())),
                FID_X_PUBLIC => super::x25519_public(arg_blob(&args, 0, "x25519_public")?)
                    .map(SqlValue::Blob),
                FID_X_SHARED => super::x25519_shared(
                    arg_blob(&args, 0, "x25519_shared")?,
                    arg_blob(&args, 1, "x25519_shared")?,
                )
                .map(SqlValue::Blob),
                FID_CHACHA_ENC => super::chacha20poly1305_encrypt(
                    arg_blob(&args, 0, "chacha20poly1305_encrypt")?,
                    arg_blob(&args, 1, "chacha20poly1305_encrypt")?,
                    arg_blob(&args, 2, "chacha20poly1305_encrypt")?,
                    arg_blob(&args, 3, "chacha20poly1305_encrypt")?,
                )
                .map(SqlValue::Blob),
                FID_CHACHA_DEC => super::chacha20poly1305_decrypt(
                    arg_blob(&args, 0, "chacha20poly1305_decrypt")?,
                    arg_blob(&args, 1, "chacha20poly1305_decrypt")?,
                    arg_blob(&args, 2, "chacha20poly1305_decrypt")?,
                    arg_blob(&args, 3, "chacha20poly1305_decrypt")?,
                )
                .map(SqlValue::Blob),
                FID_MERKLE_ROOT => {
                    let leaves = arg_blob(&args, 0, "merkle_root")?;
                    let lsz = arg_int(&args, 1, "merkle_root")? as usize;
                    super::merkle_root(leaves, lsz).map(SqlValue::Blob)
                }
                FID_MERKLE_VERIFY => Ok(SqlValue::Integer(super::merkle_proof_verify(
                    arg_blob(&args, 0, "merkle_proof_verify")?,
                    arg_blob(&args, 1, "merkle_proof_verify")?,
                    arg_blob(&args, 2, "merkle_proof_verify")?,
                ) as i64)),
                other => Err(format!("crypto-keys: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
