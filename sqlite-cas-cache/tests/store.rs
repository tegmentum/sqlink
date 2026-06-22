//! Unit tests for `SqliteCasStore` in external mode + internal mode.

use sqlite_cas_cache::{Hash, SqliteCasStore};
use sqlink_core::db::{Connection, OpenFlags};

fn fresh_external() -> (tempfile::TempDir, SqliteCasStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cas.sqlite");
    let store = SqliteCasStore::open_external(&path).expect("open external");
    (dir, store)
}

fn fresh_internal() -> (tempfile::TempDir, SqliteCasStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("user.db");
    let path_str = path.to_str().unwrap().to_string();
    let conn = Connection::open(&path_str, OpenFlags::DEFAULT).unwrap();
    // Stand in for a user's working db: install some user schema
    // first, then layer the CAS tables on top via open_internal.
    conn.execute_batch("CREATE TABLE my_user_table(x INTEGER);")
        .unwrap();
    let store = SqliteCasStore::open_internal(conn).expect("open internal");
    (dir, store)
}

#[test]
fn put_returns_blake3_of_bytes() {
    let (_d, mut s) = fresh_external();
    let bytes = b"hello world";
    let hash = s.put(bytes).unwrap();
    let expected = blake3::hash(bytes);
    assert_eq!(hash, expected);
}

#[test]
fn put_then_get_round_trips() {
    let (_d, mut s) = fresh_external();
    let bytes = b"some wasm component bytes";
    let hash = s.put(bytes).unwrap();
    let got = s.get(&hash).unwrap().expect("hit");
    assert_eq!(got, bytes);
}

#[test]
fn put_is_idempotent_on_collision() {
    let (_d, mut s) = fresh_external();
    let bytes = b"xyz";
    let h1 = s.put(bytes).unwrap();
    let h2 = s.put(bytes).unwrap();
    assert_eq!(h1, h2);
    // Still only one artifact row.
    assert_eq!(s.artifact_count().unwrap(), 1);
}

#[test]
fn get_miss_returns_none() {
    let (_d, mut s) = fresh_external();
    let missing = blake3::hash(b"never-stored");
    assert!(s.get(&missing).unwrap().is_none());
}

#[test]
fn set_uri_then_resolve_uri_round_trips() {
    let (_d, mut s) = fresh_external();
    let bytes = b"component bytes for example.com extension";
    let hash = s.put(bytes).unwrap();
    s.set_uri("https://example.com/ext.wasm", &hash).unwrap();
    let (resolved_hash, resolved_bytes) = s
        .resolve_uri("https://example.com/ext.wasm")
        .unwrap()
        .expect("hit");
    assert_eq!(resolved_hash, hash);
    assert_eq!(resolved_bytes, bytes);
}

#[test]
fn set_uri_replaces_prior_binding() {
    let (_d, mut s) = fresh_external();
    let h1 = s.put(b"v1").unwrap();
    let h2 = s.put(b"v2").unwrap();
    let uri = "https://example.com/ext.wasm";
    s.set_uri(uri, &h1).unwrap();
    s.set_uri(uri, &h2).unwrap();
    let (got, _) = s.resolve_uri(uri).unwrap().unwrap();
    assert_eq!(got, h2);
}

#[test]
fn resolve_uri_miss_returns_none() {
    let (_d, mut s) = fresh_external();
    assert!(s.resolve_uri("https://nothing.example/x").unwrap().is_none());
}

#[test]
fn list_returns_most_recently_used_first() {
    let (_d, mut s) = fresh_external();
    let a = s.put(b"a").unwrap();
    let b = s.put(b"b").unwrap();
    let c = s.put(b"c").unwrap();
    s.set_uri("u:a", &a).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    s.set_uri("u:b", &b).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    s.set_uri("u:c", &c).unwrap();
    let list = s.list().unwrap();
    assert_eq!(list.len(), 3);
    // Most-recently-used first  c, then b, then a.
    assert_eq!(list[0].uri, "u:c");
    assert_eq!(list[1].uri, "u:b");
    assert_eq!(list[2].uri, "u:a");
}

#[test]
fn total_bytes_sums_artifact_sizes() {
    let (_d, mut s) = fresh_external();
    s.put(b"abc").unwrap();
    s.put(b"defgh").unwrap();
    assert_eq!(s.total_bytes().unwrap(), 8);
}

#[test]
fn purge_clears_everything() {
    let (_d, mut s) = fresh_external();
    let h = s.put(b"abc").unwrap();
    s.set_uri("u:x", &h).unwrap();
    s.purge().unwrap();
    assert_eq!(s.total_bytes().unwrap(), 0);
    assert_eq!(s.artifact_count().unwrap(), 0);
    assert!(s.resolve_uri("u:x").unwrap().is_none());
}

#[test]
fn internal_mode_works_alongside_user_schema() {
    let (_d, mut s) = fresh_internal();
    // The user's table must coexist with __cas_* without
    // conflict.
    let h = s.put(b"internal-mode bytes").unwrap();
    s.set_uri("u:internal", &h).unwrap();
    let (got_h, got_bytes) = s.resolve_uri("u:internal").unwrap().unwrap();
    assert_eq!(got_h, h);
    assert_eq!(got_bytes, b"internal-mode bytes");
}

#[test]
fn put_populates_sha256_mirror_and_get_by_sha256_roundtrips() {
    use sha2::{Digest, Sha256};
    let (_d, mut s) = fresh_external();
    let bytes = b"sha-256 mirror payload";
    s.put(bytes).unwrap();
    let mut sha = Sha256::new();
    sha.update(bytes);
    let sha_digest: [u8; 32] = sha.finalize().into();
    let got = s.get_by_sha256(&sha_digest).unwrap();
    assert_eq!(got.as_deref(), Some(&bytes[..]));
}

#[test]
fn get_by_sha256_returns_none_for_unknown_digest() {
    let (_d, mut s) = fresh_external();
    s.put(b"known bytes").unwrap();
    let mut unknown = [0u8; 32];
    unknown[0] = 0xff; // not sha256(known bytes)
    assert!(s.get_by_sha256(&unknown).unwrap().is_none());
}

#[test]
fn external_mode_persists_across_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cas.sqlite");
    let put_hash: Hash;
    {
        let mut s = SqliteCasStore::open_external(&path).unwrap();
        put_hash = s.put(b"persistent bytes").unwrap();
        s.set_uri("u:p", &put_hash).unwrap();
    }
    // Reopen, expect data to survive.
    let mut s = SqliteCasStore::open_external(&path).unwrap();
    let (got_h, got_bytes) = s.resolve_uri("u:p").unwrap().unwrap();
    assert_eq!(got_h, put_hash);
    assert_eq!(got_bytes, b"persistent bytes");
}
