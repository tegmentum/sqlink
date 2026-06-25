//! Direct unit tests for `SqliteCasStore::bundle_*` CRUD,
//! aliasing semantics, GC policy, and binary recording.
//!
//! The full-pipeline coverage in `tests/extension-smoke/src/test_bundles.rs`
//! exercises the same surface through the wasm dispatch + bundle-cli
//! arg parser, which is slow + brittle + has no failure isolation.
//! These tests hit the storage API directly so a regression in
//! e.g. the alias path doesn't require building the wasm cli to surface.

use sqlite_cas_cache::{
    BundleAliasConflict, BundleGcPolicy, BundleMember, SqliteCasStore,
};
use sqlite_component_core::db::{StepResult, Value};

fn fresh() -> (tempfile::TempDir, SqliteCasStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteCasStore::open_external(dir.path().join("cas.sqlite"))
        .expect("open external");
    (dir, store)
}

fn member(name: &str, hash: &str) -> BundleMember {
    BundleMember {
        extension_name: name.to_string(),
        content_hash: hash.to_string(),
    }
}

/// Overwrite `last_used_at` directly so GC tests don't depend on wall-clock.
fn set_last_used(store: &SqliteCasStore, id: u64, ts: i64) {
    let mut upd = store
        .conn()
        .prepare("UPDATE __cas_bundle SET last_used_at = ?2 WHERE id = ?1")
        .unwrap();
    upd.bind_all(&[Value::Integer(id as i64), Value::Integer(ts)])
        .unwrap();
    assert!(matches!(upd.step().unwrap(), StepResult::Done));
}

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// 1. Save + roundtrip
#[test]
fn save_then_find_then_show_roundtrips() {
    let (_d, mut s) = fresh();
    let members = vec![member("uuid", "h_uuid"), member("json1", "h_json1")];
    let id = s.bundle_save(Some("myset"), "set_hash_aaaa", &members).unwrap();
    let summary = s.bundle_find_by_name("myset").unwrap().expect("found");
    assert_eq!(summary.id, id);
    assert_eq!(summary.name.as_deref(), Some("myset"));
    assert_eq!(summary.set_hash, "set_hash_aaaa");
    assert_eq!(summary.member_count, 2);
    assert_eq!(summary.binary_count, 0);
    let detail = s.bundle_show(id).unwrap().expect("shown");
    assert_eq!(detail.members.len(), 2);
    assert!(detail.binaries.is_empty());
}

// 2. Idempotent on same set_hash + same name
#[test]
fn save_is_idempotent_on_same_name_and_hash() {
    let (_d, mut s) = fresh();
    let m = vec![member("uuid", "h_uuid")];
    let id1 = s.bundle_save(Some("myset"), "set_hash_aaaa", &m).unwrap();
    let id2 = s.bundle_save(Some("myset"), "set_hash_aaaa", &m).unwrap();
    assert_eq!(id1, id2);
    assert_eq!(s.bundle_list().unwrap().len(), 1);
}

// 3. Alias conflict on name reuse with different set_hash
#[test]
fn save_errors_on_name_reuse_with_different_hash() {
    let (_d, mut s) = fresh();
    let m1 = vec![member("uuid", "h_uuid")];
    let m2 = vec![member("json1", "h_json1")];
    let _id1 = s.bundle_save(Some("myset"), "set_hash_aaaa", &m1).unwrap();
    let err = s.bundle_save(Some("myset"), "set_hash_bbbb", &m2).unwrap_err();
    let conflict = err
        .downcast_ref::<BundleAliasConflict>()
        .expect("BundleAliasConflict");
    assert_eq!(conflict.name, "myset");
    assert_eq!(conflict.existing_set_hash, "set_hash_aaaa");
    assert_eq!(conflict.new_set_hash, "set_hash_bbbb");
}

// 4. Anonymous bundles (null name)
#[test]
fn anonymous_bundle_save_and_lookup() {
    let (_d, mut s) = fresh();
    let m = vec![member("uuid", "h_uuid")];
    let id = s.bundle_save(None, "set_hash_aaaa", &m).unwrap();
    assert!(s.bundle_find_by_name("myset").unwrap().is_none());
    let by_hash = s.bundle_find_first_by_hash("set_hash_aaaa").unwrap();
    assert_eq!(by_hash.unwrap().id, id);
    let list = s.bundle_list().unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].name.is_none());
}

// 5. Hash-prefix exact (full hash) match returns 1
#[test]
fn find_by_hash_prefix_full_hash_matches_one() {
    let (_d, mut s) = fresh();
    let m = vec![member("uuid", "h_uuid")];
    let id = s.bundle_save(Some("myset"), "deadbeefaaaa", &m).unwrap();
    let hits = s.bundle_find_by_hash_prefix("deadbeefaaaa").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, id);
}

// 6. Hash-prefix ambiguous: same 4-char prefix returns both
#[test]
fn find_by_hash_prefix_ambiguous_returns_all() {
    let (_d, mut s) = fresh();
    let m = vec![member("a", "h_a")];
    let id_a = s.bundle_save(Some("a"), "4c8e_aaaaaa", &m).unwrap();
    let id_b = s.bundle_save(Some("b"), "4c8e_bbbbbb", &m).unwrap();
    let hits = s.bundle_find_by_hash_prefix("4c8e").unwrap();
    let ids: std::collections::HashSet<u64> = hits.iter().map(|h| h.id).collect();
    assert!(ids.contains(&id_a));
    assert!(ids.contains(&id_b));
    assert_eq!(ids.len(), 2);
}

// 7. Hash-prefix no-match returns empty (not error)
#[test]
fn find_by_hash_prefix_no_match_returns_empty() {
    let (_d, _s) = fresh();
    let hits = _s.bundle_find_by_hash_prefix("cafebabedead").unwrap();
    assert!(hits.is_empty());
}

// 8. Delete cascades members + binaries via FK
#[test]
fn delete_cascades_members_and_binaries() {
    let (_d, mut s) = fresh();
    let m = vec![member("uuid", "h_uuid"), member("json1", "h_json1")];
    let id = s.bundle_save(Some("myset"), "set_hash_aaaa", &m).unwrap();
    s.bundle_record_binary(id, "aarch64-apple-darwin", "/p/bin").unwrap();
    assert_eq!(s.bundle_members(id).unwrap().len(), 2);
    assert_eq!(s.bundle_binaries(id).unwrap().len(), 1);
    assert!(s.bundle_delete(id).unwrap());
    assert!(s.bundle_members(id).unwrap().is_empty());
    assert!(s.bundle_binaries(id).unwrap().is_empty());
    assert!(s.bundle_find_by_name("myset").unwrap().is_none());
}

// 9. Delete non-existent returns Ok(false), not Err
#[test]
fn delete_nonexistent_returns_false() {
    let (_d, mut s) = fresh();
    assert!(!s.bundle_delete(99_999).unwrap());
}

// 10. GC keep_last_N keeps N most-recently-used
#[test]
fn gc_keep_last_n_drops_older() {
    let (_d, mut s) = fresh();
    let m = vec![member("u", "h")];
    let id_old1 = s.bundle_save(Some("old1"), "h_old1", &m).unwrap();
    let id_old2 = s.bundle_save(Some("old2"), "h_old2", &m).unwrap();
    let id_old3 = s.bundle_save(Some("old3"), "h_old3", &m).unwrap();
    let id_new1 = s.bundle_save(Some("new1"), "h_new1", &m).unwrap();
    let id_new2 = s.bundle_save(Some("new2"), "h_new2", &m).unwrap();
    // Force a stable ordering by last_used_at.
    set_last_used(&s, id_old1, 100);
    set_last_used(&s, id_old2, 200);
    set_last_used(&s, id_old3, 300);
    set_last_used(&s, id_new1, 400);
    set_last_used(&s, id_new2, 500);

    let dropped = s
        .bundle_gc(BundleGcPolicy { keep_last: Some(2), older_than_secs: None })
        .unwrap();
    let dropped_set: std::collections::HashSet<u64> = dropped.iter().copied().collect();
    assert_eq!(dropped_set.len(), 3);
    assert!(dropped_set.contains(&id_old1));
    assert!(dropped_set.contains(&id_old2));
    assert!(dropped_set.contains(&id_old3));
    let remaining: std::collections::HashSet<u64> =
        s.bundle_list().unwrap().iter().map(|b| b.id).collect();
    assert_eq!(remaining, [id_new1, id_new2].into_iter().collect());
}

// 11. GC older-than drops only stale entries
#[test]
fn gc_older_than_drops_stale_only() {
    let (_d, mut s) = fresh();
    let m = vec![member("u", "h")];
    let id_fresh = s.bundle_save(Some("fresh"), "h_fresh", &m).unwrap();
    let id_med = s.bundle_save(Some("med"), "h_med", &m).unwrap();
    let id_stale = s.bundle_save(Some("stale"), "h_stale", &m).unwrap();
    let now = now_secs();
    set_last_used(&s, id_fresh, now - 10);
    set_last_used(&s, id_med, now - 60);
    set_last_used(&s, id_stale, now - 600);

    let dropped = s
        .bundle_gc(BundleGcPolicy { keep_last: None, older_than_secs: Some(120) })
        .unwrap();
    assert_eq!(dropped, vec![id_stale]);
    let remaining: std::collections::HashSet<u64> =
        s.bundle_list().unwrap().iter().map(|b| b.id).collect();
    assert_eq!(remaining, [id_fresh, id_med].into_iter().collect());
}

// 12. GC keep_last=0 drops everything
#[test]
fn gc_keep_last_zero_drops_all() {
    let (_d, mut s) = fresh();
    let m = vec![member("u", "h")];
    s.bundle_save(Some("a"), "h_a", &m).unwrap();
    s.bundle_save(Some("b"), "h_b", &m).unwrap();
    s.bundle_save(Some("c"), "h_c", &m).unwrap();
    let dropped = s
        .bundle_gc(BundleGcPolicy { keep_last: Some(0), older_than_secs: None })
        .unwrap();
    assert_eq!(dropped.len(), 3);
    assert!(s.bundle_list().unwrap().is_empty());
}

// 13. GC older-than=0 drops nothing (cutoff = now; nothing strictly < now)
#[test]
fn gc_older_than_zero_drops_nothing_with_now_stamps() {
    let (_d, mut s) = fresh();
    let m = vec![member("u", "h")];
    let id = s.bundle_save(Some("a"), "h_a", &m).unwrap();
    let now = now_secs();
    set_last_used(&s, id, now);
    let dropped = s
        .bundle_gc(BundleGcPolicy { keep_last: None, older_than_secs: Some(0) })
        .unwrap();
    assert!(dropped.is_empty());
    assert_eq!(s.bundle_list().unwrap().len(), 1);
}

// 14. Touch updates last_used_at
#[test]
fn touch_advances_last_used_at() {
    let (_d, mut s) = fresh();
    let m = vec![member("u", "h")];
    let id = s.bundle_save(Some("a"), "h_a", &m).unwrap();
    // Rewind so the touch has somewhere to advance from.
    set_last_used(&s, id, 100);
    let before = s.bundle_find_by_name("a").unwrap().unwrap().last_used_at;
    assert_eq!(before, 100);
    s.bundle_touch(id).unwrap();
    let after = s.bundle_find_by_name("a").unwrap().unwrap().last_used_at;
    assert!(after > before);
}

// 15. Record binary + show
#[test]
fn record_binary_then_binaries_lists_it() {
    let (_d, mut s) = fresh();
    let m = vec![member("u", "h")];
    let id = s.bundle_save(Some("a"), "h_a", &m).unwrap();
    s.bundle_record_binary(id, "aarch64-apple-darwin", "/p/bin").unwrap();
    let bins = s.bundle_binaries(id).unwrap();
    assert_eq!(bins.len(), 1);
    assert_eq!(bins[0].target_triple, "aarch64-apple-darwin");
    assert_eq!(bins[0].binary_path, "/p/bin");
    let summary = s.bundle_find_by_name("a").unwrap().unwrap();
    assert_eq!(summary.binary_count, 1);
    let detail = s.bundle_show(id).unwrap().unwrap();
    assert_eq!(detail.binaries.len(), 1);
}

// 16. Record binary idempotent per (id, target_triple): upserts path + built_at
#[test]
fn record_binary_upserts_on_same_target() {
    let (_d, mut s) = fresh();
    let m = vec![member("u", "h")];
    let id = s.bundle_save(Some("a"), "h_a", &m).unwrap();
    s.bundle_record_binary(id, "wasm32-wasip2", "/p/v1").unwrap();
    s.bundle_record_binary(id, "wasm32-wasip2", "/p/v2").unwrap();
    let bins = s.bundle_binaries(id).unwrap();
    assert_eq!(bins.len(), 1);
    assert_eq!(bins[0].binary_path, "/p/v2");
}

// 17. Members ordering: returned sorted by extension_name ascending
#[test]
fn members_returned_sorted_by_extension_name() {
    let (_d, mut s) = fresh();
    let m = vec![
        member("zz_last", "h_z"),
        member("aa_first", "h_a"),
        member("mm_middle", "h_m"),
    ];
    let id = s.bundle_save(Some("a"), "h_set", &m).unwrap();
    let got = s.bundle_members(id).unwrap();
    let names: Vec<&str> = got.iter().map(|m| m.extension_name.as_str()).collect();
    assert_eq!(names, vec!["aa_first", "mm_middle", "zz_last"]);
}

// 18. Empty members list is allowed
#[test]
fn empty_members_list_is_allowed() {
    let (_d, mut s) = fresh();
    let id = s.bundle_save(Some("empty"), "h_empty", &[]).unwrap();
    assert!(s.bundle_members(id).unwrap().is_empty());
    let summary = s.bundle_find_by_name("empty").unwrap().unwrap();
    assert_eq!(summary.member_count, 0);
}

// 19. Many members (100) round-trip
#[test]
fn many_members_round_trip() {
    let (_d, mut s) = fresh();
    let ms: Vec<BundleMember> = (0..100)
        .map(|i| member(&format!("ext_{i:03}"), &format!("h_{i:03}")))
        .collect();
    let id = s.bundle_save(Some("big"), "h_big", &ms).unwrap();
    let got = s.bundle_members(id).unwrap();
    assert_eq!(got.len(), 100);
    let summary = s.bundle_find_by_name("big").unwrap().unwrap();
    assert_eq!(summary.member_count, 100);
}

// 20. Hash-prefix lookup ordering: most-recent first
#[test]
fn find_by_hash_prefix_orders_most_recent_first() {
    let (_d, mut s) = fresh();
    let m = vec![member("u", "h")];
    let id_a = s.bundle_save(Some("a"), "4c8e_aaaaaa", &m).unwrap();
    let id_b = s.bundle_save(Some("b"), "4c8e_bbbbbb", &m).unwrap();
    let id_c = s.bundle_save(Some("c"), "4c8e_cccccc", &m).unwrap();
    set_last_used(&s, id_a, 100);
    set_last_used(&s, id_b, 300);
    set_last_used(&s, id_c, 200);
    let hits = s.bundle_find_by_hash_prefix("4c8e").unwrap();
    let ids: Vec<u64> = hits.iter().map(|h| h.id).collect();
    assert_eq!(ids, vec![id_b, id_c, id_a]);
}

// Cheap mutant kill (cargo-mutants 2026-06-25):

#[test]
fn alias_conflict_display_includes_names_and_hashes() {
    // Mutant: replace BundleAliasConflict::fmt -> std::fmt::Result with
    // Ok(Default::default()) (bundles.rs:81). The default Result<()>::default()
    // is Ok(()), and the formatter is left empty  the resulting string
    // would be "". Real impl includes the bundle name and both hashes.
    let conflict = sqlite_cas_cache::BundleAliasConflict {
        name: "myset".to_string(),
        existing_set_hash: "deadbeef".to_string(),
        new_set_hash: "cafebabe".to_string(),
    };
    let s = format!("{conflict}");
    assert!(s.contains("myset"), "missing bundle name: {s}");
    assert!(s.contains("deadbeef"), "missing existing hash: {s}");
    assert!(s.contains("cafebabe"), "missing new hash: {s}");
}
