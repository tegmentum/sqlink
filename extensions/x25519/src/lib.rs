//! X25519 ECDH key exchange (RFC 7748) for SQL.
//!
//! Function surface:
//!
//!   x25519_keypair()                       -> blob (64 bytes: 32 priv || 32 pub)
//!   x25519_pub_from_priv(priv_blob)        -> blob (32-byte public key)
//!   x25519_shared_secret(priv, pub_blob)   -> blob (32-byte shared secret)
//!   x25519_version()                       -> text
//!
//! All scalar values are 32-byte blobs.
//!
//! NULL -> NULL on every fn. Wrong-length input -> NULL (not an error),
//! so the surface composes inside CASE / WHERE.
//!
//! ## Why no on-curve validation
//!
//! For X25519 every 32-byte value is a valid Montgomery u-coordinate
//! after RFC 7748 clamping; the crate handles the clamp internally.
//! There is *no* shape check we could apply to pub or priv bytes
//! beyond `len == 32`.
//!
//! ## Low-order point caveat
//!
//! RFC 7748 § 6.1 lists 12 "small-order" public keys (including the
//! all-zero point) whose ECDH output is always the all-zero shared
//! secret regardless of the private key. We return the literal output
//! the spec dictates (32 zero bytes) -- callers needing contributory
//! key exchange should check the result is not zero. The smoke vector
//! demonstrates this explicitly.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
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

    use x25519_dalek::{PublicKey, StaticSecret};

    const FID_KEYPAIR: u64 = 1;
    const FID_PUB_FROM_PRIV: u64 = 2;
    const FID_SHARED_SECRET: u64 = 3;
    const FID_VERSION: u64 = 4;

    struct Ext;

    /// Optional-blob: NULL passes through as None; BLOB / TEXT surface as
    /// bytes; INTEGER / REAL reject (no sensible coercion for binary keys).
    fn opt_blob(args: &[SqlValue], i: usize) -> Result<Option<Vec<u8>>, String> {
        match args.get(i) {
            None => Err(format!("missing blob arg at {i}")),
            Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Blob(b)) => Ok(Some(b.clone())),
            Some(SqlValue::Text(s)) => Ok(Some(s.as_bytes().to_vec())),
            _ => Err(format!("arg at {i} must be BLOB / TEXT / NULL")),
        }
    }

    /// Coerce a byte slice to a 32-byte array, or None on wrong length.
    fn as_32(bytes: &[u8]) -> Option<[u8; 32]> {
        bytes.try_into().ok()
    }

    /// Derive the X25519 public key from a 32-byte private blob.
    /// Clamping is applied implicitly by the dalek API.
    fn pub_from_priv(priv_bytes: [u8; 32]) -> [u8; 32] {
        let sk = StaticSecret::from(priv_bytes);
        let pk: PublicKey = (&sk).into();
        pk.to_bytes()
    }

    /// Compute the ECDH shared secret. Note: low-order public keys
    /// produce the all-zero output; the spec defines this and we surface
    /// it faithfully. Callers needing contributory DH check the output.
    fn shared_secret(priv_bytes: [u8; 32], pub_bytes: [u8; 32]) -> [u8; 32] {
        let sk = StaticSecret::from(priv_bytes);
        let pk = PublicKey::from(pub_bytes);
        sk.diffie_hellman(&pk).to_bytes()
    }

    /// Generate a fresh 32-byte private key from wasi:random/random and
    /// return (priv || pub) as a single 64-byte blob. The priv bytes are
    /// the raw RNG output (clamping happens at use-time inside dalek).
    fn gen_keypair() -> Result<Vec<u8>, String> {
        let mut priv_bytes = [0u8; 32];
        getrandom::getrandom(&mut priv_bytes)
            .map_err(|e| format!("x25519_keypair: rng: {e}"))?;
        let pub_bytes = pub_from_priv(priv_bytes);
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(&priv_bytes);
        out.extend_from_slice(&pub_bytes);
        Ok(out)
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            // Random keypair must not be folded across rows.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "x25519".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_KEYPAIR, "x25519_keypair", 0, nd),
                    s(FID_PUB_FROM_PRIV, "x25519_pub_from_priv", 1, det),
                    s(FID_SHARED_SECRET, "x25519_shared_secret", 2, det),
                    s(FID_VERSION, "x25519_version", 0, det),
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
                FID_KEYPAIR => Ok(SqlValue::Blob(gen_keypair()?)),

                FID_PUB_FROM_PRIV => {
                    let Some(b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match as_32(&b) {
                        Some(arr) => SqlValue::Blob(pub_from_priv(arr).to_vec()),
                        None => SqlValue::Null,
                    })
                }

                FID_SHARED_SECRET => {
                    let Some(priv_b) = opt_blob(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(pub_b) = opt_blob(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(priv_arr) = as_32(&priv_b) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(pub_arr) = as_32(&pub_b) else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Blob(shared_secret(priv_arr, pub_arr).to_vec()))
                }

                FID_VERSION => {
                    let v = format!(
                        "x25519-dalek 2.0 (RFC 7748); extension {}",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }

                other => Err(format!("x25519: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
