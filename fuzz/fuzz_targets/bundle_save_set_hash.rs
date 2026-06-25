#![no_main]
//! Fuzz `SqliteCasStore::bundle_save`. Properties:
//!   1. bundle_save never panics for arbitrary (name, set_hash, members).
//!   2. Idempotent on (name, set_hash) — calling twice yields the same id.
//!   3. Alias conflict detection: same name + different set_hash errors,
//!      never produces a duplicate row.
//!
//! The bundle-cli wrapper computes set_hash via blake3 of sorted
//! (name, digest) pairs. This target fuzzes the *storage* path
//! that wrapper feeds — set_hash is treated as opaque text by
//! the store, so we vary it freely.

use libfuzzer_sys::fuzz_target;
use arbitrary::{Arbitrary, Unstructured};
use sqlite_cas_cache::{BundleMember, SqliteCasStore};

#[derive(Arbitrary, Debug)]
struct Input {
    name: Option<String>,
    set_hash: String,
    members: Vec<(String, String)>,
    // Second-call hash to exercise the alias-conflict branch.
    alt_set_hash: Option<String>,
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(input) = Input::arbitrary(&mut u) else { return };

    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = SqliteCasStore::open_external(dir.path().join("cas.sqlite"))
        .expect("open cas");

    let members: Vec<BundleMember> = input
        .members
        .iter()
        .map(|(n, h)| BundleMember {
            extension_name: n.clone(),
            content_hash: h.clone(),
        })
        .collect();

    let id1 = match store.bundle_save(input.name.as_deref(), &input.set_hash, &members) {
        Ok(id) => id,
        Err(_) => return, // accepted error path (e.g. SQL constraint); not a panic
    };

    // Property: idempotent on (name, set_hash) within one session.
    let id2 = store
        .bundle_save(input.name.as_deref(), &input.set_hash, &members)
        .expect("idempotent save");
    assert_eq!(id1, id2, "bundle_save not idempotent");

    // Property: alias-conflict branch never panics, never silently
    // creates a duplicate row for the same name.
    if let (Some(name), Some(alt)) = (input.name.as_deref(), input.alt_set_hash.as_deref()) {
        if alt != input.set_hash {
            let res = store.bundle_save(Some(name), alt, &members);
            // Either err (expected — alias conflict) or ok with same id
            // (the storage layer accepted alt as a no-op rebind).
            if let Ok(id3) = res {
                assert_eq!(id3, id1, "alias rebind produced different id");
            }
        }
    }
});
