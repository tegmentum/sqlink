//! Tests for `gc` and `evict_lru`.

use sqlite_cas_cache::SqliteCasStore;

fn fresh() -> (tempfile::TempDir, SqliteCasStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteCasStore::open_external(dir.path().join("cas.sqlite")).unwrap();
    (dir, store)
}

#[test]
fn gc_drops_unbound_artifacts() {
    let (_d, mut s) = fresh();
    let bound = s.put(b"bound-bytes-aaa").unwrap();
    let _unbound = s.put(b"unbound-bytes-bbbbb").unwrap();
    s.set_uri("u:bound", &bound).unwrap();
    assert_eq!(s.artifact_count().unwrap(), 2);
    let freed = s.gc().unwrap();
    assert_eq!(freed, b"unbound-bytes-bbbbb".len() as u64);
    assert_eq!(s.artifact_count().unwrap(), 1);
    // Bound artifact + its URI still resolvable.
    assert!(s.resolve_uri("u:bound").unwrap().is_some());
}

#[test]
fn gc_keeps_artifacts_with_at_least_one_uri() {
    let (_d, mut s) = fresh();
    let h = s.put(b"shared").unwrap();
    s.set_uri("u:a", &h).unwrap();
    s.set_uri("u:b", &h).unwrap();
    s.gc().unwrap();
    assert_eq!(s.artifact_count().unwrap(), 1);
    // Drop one URI; artifact still kept.
    assert!(s.delete_uri("u:a").unwrap());
    s.gc().unwrap();
    assert_eq!(s.artifact_count().unwrap(), 1);
    // Drop the last URI; gc collects.
    assert!(s.delete_uri("u:b").unwrap());
    s.gc().unwrap();
    assert_eq!(s.artifact_count().unwrap(), 0);
}

#[test]
fn delete_uri_returns_false_when_missing() {
    let (_d, mut s) = fresh();
    assert!(!s.delete_uri("u:nothing").unwrap());
}

#[test]
fn evict_lru_below_target_is_noop() {
    let (_d, mut s) = fresh();
    s.put(b"abc").unwrap();
    let before = s.total_bytes().unwrap();
    let freed = s.evict_lru(before + 1000).unwrap();
    assert_eq!(freed, 0);
    assert_eq!(s.total_bytes().unwrap(), before);
}

#[test]
fn evict_lru_drops_oldest_uri_first() {
    let (_d, mut s) = fresh();
    let h1 = s.put(b"AAAAAAAA").unwrap(); // 8 bytes
    s.set_uri("u:old", &h1).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let h2 = s.put(b"BBBBBBBB").unwrap(); // 8 bytes
    s.set_uri("u:new", &h2).unwrap();
    assert_eq!(s.total_bytes().unwrap(), 16);
    // Squeeze down to 10 bytes: oldest URI ("u:old") should go,
    // freeing 8 bytes  total = 8.
    let freed = s.evict_lru(10).unwrap();
    assert_eq!(freed, 8);
    assert_eq!(s.total_bytes().unwrap(), 8);
    assert!(s.resolve_uri("u:old").unwrap().is_none());
    assert!(s.resolve_uri("u:new").unwrap().is_some());
}

#[test]
fn evict_lru_can_drive_to_zero() {
    let (_d, mut s) = fresh();
    let h1 = s.put(b"xxxx").unwrap();
    let h2 = s.put(b"yyyy").unwrap();
    s.set_uri("u:1", &h1).unwrap();
    s.set_uri("u:2", &h2).unwrap();
    let freed = s.evict_lru(0).unwrap();
    assert_eq!(freed, 8);
    assert_eq!(s.total_bytes().unwrap(), 0);
    assert_eq!(s.artifact_count().unwrap(), 0);
}

#[test]
fn evict_lru_drops_unbound_artifacts_after_uris_gone() {
    let (_d, mut s) = fresh();
    // Bound artifact (3 bytes).
    let bound = s.put(b"BND").unwrap();
    s.set_uri("u:bound", &bound).unwrap();
    // Unbound artifact (5 bytes).
    let _ub = s.put(b"UBOND").unwrap();
    assert_eq!(s.total_bytes().unwrap(), 8);
    // Target = 4: drop the unbound 5-byte artifact first (lower
    // last_used_at is... actually both were put within the same
    // second, so ties broken by hash ASC. Either way the unbound
    // one is preferentially droppable). Total should be 3.
    let freed = s.evict_lru(4).unwrap();
    assert_eq!(freed, 5);
    assert_eq!(s.total_bytes().unwrap(), 3);
    assert!(s.resolve_uri("u:bound").unwrap().is_some());
}

#[test]
fn evict_lru_keeps_shared_artifact_until_all_uris_drop() {
    let (_d, mut s) = fresh();
    let shared = s.put(b"shared-bytes-xxx").unwrap(); // 16 bytes
    s.set_uri("u:a", &shared).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    s.set_uri("u:b", &shared).unwrap();
    assert_eq!(s.total_bytes().unwrap(), 16);
    assert_eq!(s.artifact_count().unwrap(), 1);
    // Target = 10: must drop both URIs to free the shared
    // artifact.
    let freed = s.evict_lru(10).unwrap();
    assert_eq!(freed, 16);
    assert_eq!(s.artifact_count().unwrap(), 0);
}

// Mutant boundary tests: evict_lru phase loops use `> target_bytes`,
// not `>= target_bytes`. The distinguishing case is when total exactly
// equals target after some eviction; `>` stops, `>=` evicts one more.

#[test]
fn evict_lru_stops_at_exact_target_unbound() {
    let (_d, mut s) = fresh();
    let _a = s.put(b"aaaa").unwrap();
    let _b = s.put(b"bb").unwrap();
    let total = s.total_bytes().unwrap();
    assert_eq!(total, 6);
    // Target 4: evict_lru should drop the 2-byte artifact (oldest is
    // first-inserted under same last_used_at, but pick may pick either;
    // either way total ends at 4 = exact target, NOT below).
    let freed = s.evict_lru(4).unwrap();
    assert_eq!(s.total_bytes().unwrap(), 4);
    assert_eq!(freed, 2);
}

#[test]
fn evict_lru_stops_at_exact_target_uri_phase() {
    let (_d, mut s) = fresh();
    let h = s.put(b"shared12").unwrap();
    s.set_uri("u:a", &h).unwrap();
    s.set_uri("u:b", &h).unwrap();
    assert_eq!(s.total_bytes().unwrap(), 8);
    // Both URIs reference same artifact; gc keeps artifact alive while
    // either URI exists. Phase 2 drops URIs oldest-first until target.
    // Target 8 = exact: early-return path, freed == 0.
    let freed = s.evict_lru(8).unwrap();
    assert_eq!(s.total_bytes().unwrap(), 8);
    assert_eq!(freed, 0);
}
