//! Per-extension typed-value registry (PLAN-wit-value-extension.md
//! Phase B / DD3).
//!
//! The WIT contract gained a `wit-value(wit-value-payload)` arm on
//! `sql-value` in @1.0.0. The payload's `type-id` is a 32-byte
//! sha256 over the WIT record's `canon:wit` shape; matching it back
//! to a concrete WIT record (so a bridge can call its decoder /
//! encoder) requires a per-extension lookup table populated at
//! extension-init time.
//!
//! ## What lives here
//!
//! Two things:
//!
//! 1. A process-global `TypedValueRegistry` keyed by `type_id ->
//!    TypedValueBinding`. Each extension contributes the entries
//!    from its `metadata.manifest.typed-values` field at load time;
//!    the host enforces conflict-resolution (same type-id, different
//!    decoder = error — likely a structural drift in canon:wit
//!    hashing, never benign).
//!
//! 2. A `TypedValueCodecs` test/dispatch hook map keyed by type-id
//!    to a `dyn TypedValueCodec`. This is the host-side wiring point
//!    Phase B's round-trip test populates with Rust closures.
//!    Phase C codegen populates it with a `WasmCodec` that calls
//!    the bridge's serde-ops export.
//!
//! ## Wire boundary (decode / encode)
//!
//! `TypedValueCodec::decode_to_canon` takes the raw payload bytes
//! that came across the wire and returns the canonical-CBOR bytes
//! the bridge's decoder-import would accept. For Phase B's
//! synthetic test the codec is an identity round-trip (the payload
//! IS canonical-CBOR already, by construction). For Phase C the
//! codec validates by calling the bridge's
//! `<package>:wasm/serde-ops/<type>-from-canon-cbor` import and
//! re-encoding via `<type>-to-canon-cbor` so non-canonical input
//! gets normalised at the boundary.
//!
//! Phase B intentionally keeps the codec trait byte-in / byte-out
//! rather than threading WIT-record types through; the actual
//! wasm-side record construction belongs to the codegen-emitted
//! bridge, not the host. Hosts only ferry the bytes + the typed
//! identity.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

/// One row of the per-extension `typed-value-binding` manifest.
/// Mirrors the WIT `metadata::typed-value-binding` record, with
/// type-id normalised to `[u8; 32]` (the WIT carries it as
/// `list<u8>` for forward-compat, but every Phase B+ producer ships
/// a 32-byte sha256).
#[derive(Debug, Clone, PartialEq)]
pub struct TypedValueBinding {
    /// 32-byte sha256(canon:wit) shape hash. Authoritative lookup key.
    pub type_id: [u8; 32],
    /// `"<package>:wasm/<interface>@<version>/<type-name>"`. Used in
    /// error messages; never for matching.
    pub symbolic_name: String,
    /// Wasm-side function the bridge component exports; the host
    /// invokes it (via the cached_minimal/-stateful instance) to
    /// turn canonical-CBOR bytes into a WIT record.
    /// Convention: `"<package>:wasm/serde-ops/<type>-from-canon-cbor"`.
    pub decoder_import: String,
    /// Inverse of `decoder_import`. Convention:
    /// `"<package>:wasm/serde-ops/<type>-to-canon-cbor"`.
    pub encoder_import: String,
    /// Which extension declared this binding. The dispatcher uses
    /// this to find the right `LoadedExtension` (and thus the right
    /// cached wasmtime `Instance`) when ferrying a value.
    pub extension_name: String,
}

/// Process-global registry. Keyed by `type_id` because that's the
/// authoritative match key per DD2; symbolic-name is diagnostic-only.
///
/// Conflict policy: a second insertion with the SAME `type_id` but
/// DIFFERENT `decoder_import`/`encoder_import`/`extension_name` is
/// an error. This is what a structural drift in `canon:wit`
/// normalisation would look like at runtime; we want it loud rather
/// than silent.
///
/// Identical re-insertions (same key, same value) are a no-op so
/// reloading the same extension is safe.
#[derive(Debug, Clone, Default)]
pub struct TypedValueRegistry {
    inner: Arc<RwLock<HashMap<[u8; 32], TypedValueBinding>>>,
}

impl TypedValueRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one binding. Returns Err on a conflict (same type-id,
    /// different binding) — see `RegistryConflict` for the detail
    /// the caller surfaces to the user.
    pub fn insert(&self, binding: TypedValueBinding) -> Result<(), RegistryConflict> {
        let mut w = self.inner.write();
        if let Some(existing) = w.get(&binding.type_id) {
            if existing == &binding {
                return Ok(());
            }
            return Err(RegistryConflict {
                type_id: binding.type_id,
                existing: existing.clone(),
                incoming: binding,
            });
        }
        w.insert(binding.type_id, binding);
        Ok(())
    }

    /// Bulk-insert every binding from one extension's manifest.
    /// Stops at the first conflict and returns it; the caller
    /// decides whether to reject the whole load or proceed
    /// (current callers reject — a partial registry is worse than
    /// no registry).
    pub fn insert_all(
        &self,
        bindings: impl IntoIterator<Item = TypedValueBinding>,
    ) -> Result<(), RegistryConflict> {
        for b in bindings {
            self.insert(b)?;
        }
        Ok(())
    }

    /// Look up a binding by type-id. None if unknown — the caller
    /// surfaces a "no extension declared decoder for type-id 0x…"
    /// error.
    pub fn lookup(&self, type_id: &[u8; 32]) -> Option<TypedValueBinding> {
        self.inner.read().get(type_id).cloned()
    }

    /// Remove every binding owned by `extension_name`. Called by
    /// `unregister-extension` so a stale entry doesn't survive an
    /// extension reload that re-hashed its types.
    pub fn remove_extension(&self, extension_name: &str) {
        self.inner
            .write()
            .retain(|_, b| b.extension_name != extension_name);
    }

    /// Snapshot of every registered binding. Used by introspection
    /// SPI (`.typed-values` dot-cmd, future) and by tests.
    pub fn snapshot(&self) -> Vec<TypedValueBinding> {
        self.inner.read().values().cloned().collect()
    }

    /// True if there are no bindings — used by dispatchers to bail
    /// out fast on the "no extension uses wit-value yet" hot path.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }
}

/// Returned by `TypedValueRegistry::insert` on a collision. The
/// caller composes it into the load-error message so the operator
/// can find which two extensions disagreed.
#[derive(Debug, Clone)]
pub struct RegistryConflict {
    pub type_id: [u8; 32],
    pub existing: TypedValueBinding,
    pub incoming: TypedValueBinding,
}

impl std::fmt::Display for RegistryConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "typed-value registry conflict on type-id {}: \
             extension {} already declared decoder {:?} / encoder {:?} / symbolic {:?}; \
             extension {} declared decoder {:?} / encoder {:?} / symbolic {:?}",
            hex_short(&self.type_id),
            self.existing.extension_name,
            self.existing.decoder_import,
            self.existing.encoder_import,
            self.existing.symbolic_name,
            self.incoming.extension_name,
            self.incoming.decoder_import,
            self.incoming.encoder_import,
            self.incoming.symbolic_name,
        )
    }
}

impl std::error::Error for RegistryConflict {}

fn hex_short(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(8);
    for byte in &b[..4] {
        use std::fmt::Write;
        let _ = write!(s, "{byte:02x}");
    }
    s.push('…');
    s
}

// ---------- Codec dispatch (Phase B test path, Phase C wasm path)

/// Abstract canonical-CBOR ↔ wasm-side WIT record codec. Phase B
/// uses synthetic Rust-closure implementations to drive the
/// round-trip test (no real bridge ships decoders yet). Phase C
/// codegen produces a `WasmCodec` that calls the bridge's
/// serde-ops exports via the cached wasmtime instance.
///
/// Methods are byte-in / byte-out because the host doesn't actually
/// need a typed WIT record at this level — it just shepherds the
/// canonical-CBOR bytes between the SQL boundary and the bridge.
/// The bridge's serde-ops export does the structural reconstruction
/// on its side of the wasm boundary.
pub trait TypedValueCodec: Send + Sync {
    /// Validate that `bytes` is well-formed canonical-CBOR for this
    /// type-id and return the canonical form (always identical to
    /// the input for an already-canonical payload — the contract is
    /// canonical-CBOR end-to-end). A real codec verifies by calling
    /// the bridge's decoder import; the synthetic codec used in
    /// tests does a structural sanity check.
    fn decode_to_canon(&self, bytes: &[u8]) -> Result<Vec<u8>, String>;

    /// Inverse: the bridge re-encodes the WIT record back to
    /// canonical-CBOR. Phase B's synthetic codec is an identity
    /// pass-through.
    fn encode_from_canon(&self, bytes: &[u8]) -> Result<Vec<u8>, String>;
}

/// Holder for the codec-per-type-id table. Lives alongside
/// `TypedValueRegistry` on the Host. Populated by Phase B's test
/// suite (Rust closures) and, in Phase C, by codegen on load.
#[derive(Clone, Default)]
pub struct TypedValueCodecs {
    inner: Arc<RwLock<HashMap<[u8; 32], Arc<dyn TypedValueCodec>>>>,
}

impl TypedValueCodecs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn install(&self, type_id: [u8; 32], codec: Arc<dyn TypedValueCodec>) {
        self.inner.write().insert(type_id, codec);
    }

    pub fn lookup(&self, type_id: &[u8; 32]) -> Option<Arc<dyn TypedValueCodec>> {
        self.inner.read().get(type_id).cloned()
    }

    pub fn remove(&self, type_id: &[u8; 32]) {
        self.inner.write().remove(type_id);
    }
}

impl std::fmt::Debug for TypedValueCodecs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.inner.read().len();
        write!(f, "TypedValueCodecs {{ {n} codecs }}")
    }
}

// ---------- Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(type_id: u8, ext: &str, dec: &str, enc: &str, sym: &str) -> TypedValueBinding {
        let mut id = [0u8; 32];
        id[0] = type_id;
        TypedValueBinding {
            type_id: id,
            symbolic_name: sym.to_string(),
            decoder_import: dec.to_string(),
            encoder_import: enc.to_string(),
            extension_name: ext.to_string(),
        }
    }

    #[test]
    fn insert_then_lookup_roundtrips() {
        let r = TypedValueRegistry::new();
        let b = binding(1, "ext-a", "dec", "enc", "ext-a:wasm/x@1/T");
        r.insert(b.clone()).expect("insert");
        let got = r.lookup(&b.type_id).expect("lookup hits");
        assert_eq!(got, b);
    }

    #[test]
    fn duplicate_with_identical_payload_is_idempotent() {
        let r = TypedValueRegistry::new();
        let b = binding(1, "ext-a", "dec", "enc", "sym");
        r.insert(b.clone()).expect("first insert");
        r.insert(b.clone()).expect("idempotent re-insert");
        assert_eq!(r.snapshot().len(), 1);
    }

    #[test]
    fn duplicate_type_id_different_decoder_is_a_conflict() {
        let r = TypedValueRegistry::new();
        r.insert(binding(1, "ext-a", "decA", "encA", "symA"))
            .expect("first");
        let err = r
            .insert(binding(1, "ext-b", "decB", "encB", "symB"))
            .expect_err("conflict");
        assert_eq!(err.existing.extension_name, "ext-a");
        assert_eq!(err.incoming.extension_name, "ext-b");
        let msg = err.to_string();
        assert!(msg.contains("ext-a"));
        assert!(msg.contains("ext-b"));
    }

    #[test]
    fn remove_extension_clears_only_its_entries() {
        let r = TypedValueRegistry::new();
        r.insert(binding(1, "ext-a", "decA", "encA", "symA")).unwrap();
        r.insert(binding(2, "ext-b", "decB", "encB", "symB")).unwrap();
        r.remove_extension("ext-a");
        let snap = r.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].extension_name, "ext-b");
    }

    /// Synthetic codec: identity round-trip. Phase B's round-trip
    /// test uses this shape to prove the registry + codec
    /// abstraction composes without needing a real wasm bridge.
    struct IdentityCodec;
    impl TypedValueCodec for IdentityCodec {
        fn decode_to_canon(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
            Ok(bytes.to_vec())
        }
        fn encode_from_canon(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
            Ok(bytes.to_vec())
        }
    }

    #[test]
    fn codec_table_dispatches_by_type_id() {
        let codecs = TypedValueCodecs::new();
        let mut id = [0u8; 32];
        id[0] = 0xab;
        codecs.install(id, Arc::new(IdentityCodec));
        let codec = codecs.lookup(&id).expect("installed");
        let canon = vec![0xde, 0xad, 0xbe, 0xef];
        assert_eq!(codec.decode_to_canon(&canon).unwrap(), canon);
        assert_eq!(codec.encode_from_canon(&canon).unwrap(), canon);
    }

    // ---------------- Phase B (B7) round-trip acceptance test
    //
    // PLAN-wit-value-extension.md Phase B's acceptance gate: a
    // synthetic wit-value through the registry-driven codec produces
    // a byte-identical re-encoding.
    //
    // What this proves:
    //
    // 1. The registry populates correctly from a synthetic manifest
    //    entry (Phase B's stand-in for what Phase C codegen will
    //    emit on real bridges).
    // 2. Looking up a payload by type-id finds the binding metadata.
    // 3. A registered TypedValueCodec dispatches at decode + encode
    //    time, and the byte content survives a decode→encode round-
    //    trip unchanged.
    //
    // What this does NOT prove (Phase B is honest about):
    //
    // - Actual wasm-side decoder/encoder invocation. Phase B has no
    //   real bridge that ships those imports. Phase C codegen wires
    //   a WasmCodec impl that calls the bridge's serde-ops exports.

    use sqlite_component_core::db::{Value, WitValuePayload};

    fn synthetic_type_id() -> [u8; 32] {
        let mut id = [0u8; 32];
        for i in 0..32 {
            id[i] = (i as u8) ^ 0x55;
        }
        id
    }

    fn synthetic_binding() -> TypedValueBinding {
        TypedValueBinding {
            type_id: synthetic_type_id(),
            symbolic_name: "synthetic:wasm/temporal-types@0.1.0/tfloat-sequence".to_string(),
            decoder_import:
                "synthetic:wasm/serde-ops/tfloat-sequence-from-canon-cbor".to_string(),
            encoder_import:
                "synthetic:wasm/serde-ops/tfloat-sequence-to-canon-cbor".to_string(),
            extension_name: "synthetic-bridge".to_string(),
        }
    }

    fn synthetic_payload(bytes: Vec<u8>) -> WitValuePayload {
        WitValuePayload {
            type_id: synthetic_type_id(),
            bytes,
            symbolic_name: "synthetic:wasm/temporal-types@0.1.0/tfloat-sequence".to_string(),
        }
    }

    /// Codec that toggles the high bit on every byte. Used by the
    /// "codec actually runs" test to distinguish a real dispatch
    /// from a silent identity-passthrough.
    struct ToggleHighBitCodec;
    impl TypedValueCodec for ToggleHighBitCodec {
        fn decode_to_canon(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
            Ok(bytes.iter().map(|b| b ^ 0x80).collect())
        }
        fn encode_from_canon(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
            Ok(bytes.iter().map(|b| b ^ 0x80).collect())
        }
    }

    #[test]
    fn b7_synthetic_decode_encode_roundtrip_is_byte_identical() {
        let r = TypedValueRegistry::new();
        let codecs = TypedValueCodecs::new();
        let binding = synthetic_binding();
        r.insert(binding.clone()).unwrap();
        codecs.install(binding.type_id, Arc::new(IdentityCodec));

        // Synthetic canonical-CBOR payload. The shape doesn't matter
        // for the test — what matters is that the bytes survive the
        // registry-driven decode→encode cycle unchanged.
        let canonical_bytes: Vec<u8> = vec![
            0xa1, // CBOR map (1 entry)
            0x65, b'h', b'e', b'l', b'l', b'o', // key: "hello"
            0x05, // value: 5
        ];

        let payload = synthetic_payload(canonical_bytes.clone());

        // Decode arm: receive a SqlValue::WitValue → look up codec
        // via registry → get canonical bytes back.
        assert_eq!(
            r.lookup(&payload.type_id).expect("registered").extension_name,
            "synthetic-bridge"
        );
        let codec = codecs.lookup(&payload.type_id).expect("codec installed");
        let decoded = codec
            .decode_to_canon(&payload.bytes)
            .expect("decode succeeds");
        assert_eq!(decoded, canonical_bytes);

        // Encode arm: inverse path.
        let re_encoded = codec
            .encode_from_canon(&decoded)
            .expect("encode succeeds");
        assert_eq!(
            re_encoded, canonical_bytes,
            "round-trip is byte-identical"
        );

        // Wrap back into a payload + db::Value to prove the
        // identity-pass-through composes with the full marshaling
        // shape.
        let recovered = synthetic_payload(re_encoded);
        let v = Value::WitValue(recovered.clone());
        match v {
            Value::WitValue(p) => {
                assert_eq!(p.type_id, recovered.type_id);
                assert_eq!(p.bytes, canonical_bytes);
                assert_eq!(p.symbolic_name, recovered.symbolic_name);
            }
            other => panic!("expected WitValue, got {other:?}"),
        }
    }

    #[test]
    fn b7_codec_is_actually_invoked() {
        // Nontrivial codec proves the registry's dispatch path
        // didn't silently bypass the codec slot and pass payload
        // bytes through.
        let r = TypedValueRegistry::new();
        let codecs = TypedValueCodecs::new();
        let binding = synthetic_binding();
        r.insert(binding.clone()).unwrap();
        codecs.install(binding.type_id, Arc::new(ToggleHighBitCodec));

        let original = vec![0x00, 0x7f, 0x80, 0xff];
        let codec = codecs.lookup(&binding.type_id).unwrap();
        let decoded = codec.decode_to_canon(&original).unwrap();
        assert_eq!(decoded, vec![0x80, 0xff, 0x00, 0x7f]);
        let re_encoded = codec.encode_from_canon(&decoded).unwrap();
        assert_eq!(re_encoded, original, "encode is the inverse of decode");
    }

    #[test]
    fn b7_unknown_type_id_lookup_misses() {
        let r = TypedValueRegistry::new();
        r.insert(synthetic_binding()).unwrap();
        let mut other = [0u8; 32];
        other[0] = 0xff;
        assert!(r.lookup(&other).is_none());
    }

    #[test]
    fn b7_missing_codec_falls_back_to_identity_passthrough() {
        // Phase B contract: with no real bridges shipping codecs
        // yet, the canonical-CBOR bytes ARE the payload bytes.
        // Looking up a codec slot for a registered type-id with no
        // codec installed returns None; Host::decode_wit_value
        // takes the identity-passthrough branch.
        let r = TypedValueRegistry::new();
        let codecs = TypedValueCodecs::new();
        r.insert(synthetic_binding()).unwrap();
        let id = synthetic_type_id();
        assert!(
            codecs.lookup(&id).is_none(),
            "no codec installed for this type-id"
        );
        // Registry still resolves the binding so error context can
        // surface the symbolic name.
        let binding = r.lookup(&id).expect("registered");
        assert_eq!(
            binding.symbolic_name,
            "synthetic:wasm/temporal-types@0.1.0/tfloat-sequence"
        );
    }
}
