//! Vendored SQL string surface for the CAS-cache bundle registry.
//! Mirrors `sqlite_cas_cache::bundles_exec::*_SQL`. Kept in sync
//! with that single source of truth; bundle-cli is wasm32-wasip2
//! and can't depend on the host's sqlite-cas-cache crate (which
//! pulls in libsqlite3-sys + the cache file paths), so the strings
//! get vendored here at build time.
//!
//! These constants feed into
//! `dispatch_bridge_cas::bridged_execute_cas(sql, params)`. The
//! native sqlink-host's `dispatch_bridge_cas::Host` impl routes
//! each call to `Cache::with_bundles_conn`; the browser composed
//! binary's `sqlink:wasm/dispatch-bridge-cas` impl routes to
//! sqlite-lib's in-WASM `cas_with`. Same SQL surface, different
//! Connection — exactly the v1.5 round 2 unify-cutover shape.

// Single source of truth: `sqlite-cas-cache/src/bundles_exec.rs`.
// On the host side those consts are referenced directly (path-dep
// crate); here on the wasm side they're vendored. When updating,
// touch both — the doc comments in bundles_exec.rs are the
// authoritative reference for each statement's bind parameters and
// return columns.

#![allow(dead_code)]

/// `?1` = optional bundle name (TEXT or NULL), `?2` = set_hash
/// (TEXT), `?3` = now (INTEGER, unix seconds). Inserts a new row
/// into `__cas_bundle`; the caller reads `last_insert_rowid` for
/// the new bundle id. Schema sets `created_at = ?3` and
/// `last_used_at = ?3`.
pub const BUNDLE_INSERT_SQL: &str =
    "INSERT INTO __cas_bundle(name, set_hash, created_at, last_used_at) \
     VALUES(?1, ?2, ?3, ?3)";

/// `?1` = bundle_id (INTEGER), `?2` = extension_name (TEXT),
/// `?3` = content_hash (TEXT). Inserts one member row per bundle.
pub const MEMBER_INSERT_SQL: &str =
    "INSERT INTO __cas_bundle_member(bundle_id, extension_name, content_hash) \
     VALUES(?1, ?2, ?3)";

/// `?1` = alias name (TEXT). Returns 0-or-1 row with the
/// `bundle_id` (INTEGER) it points at.
pub const ALIAS_FIND_SQL: &str =
    "SELECT bundle_id FROM __cas_bundle_alias WHERE name = ?1";

/// `?1` = alias name (TEXT), `?2` = bundle_id (INTEGER),
/// `?3` = now (INTEGER, unix seconds). Inserts a new alias row.
pub const ALIAS_INSERT_SQL: &str =
    "INSERT INTO __cas_bundle_alias(name, bundle_id, created_at) \
     VALUES(?1, ?2, ?3)";

/// `?1` = alias name (TEXT). Removes the alias. `changes` is 1
/// iff a row was actually deleted (the host's wrapper uses
/// `total_changes()` to disambiguate from concurrent writes).
pub const ALIAS_DELETE_SQL: &str =
    "DELETE FROM __cas_bundle_alias WHERE name = ?1";

/// `?1` = bundle_id (INTEGER). Returns rows of
/// `(name TEXT)` for every alias pointing at this bundle.
pub const ALIASES_LIST_SQL: &str =
    "SELECT name FROM __cas_bundle_alias WHERE bundle_id = ?1 ORDER BY name";

/// `?1` = name (TEXT). Returns 0-or-1 row of the bundle summary
/// columns `(id INTEGER, name TEXT|NULL, set_hash TEXT,
/// created_at INTEGER, last_used_at INTEGER)`. Resolves both
/// direct bundle names and aliases via a LEFT JOIN through
/// `__cas_bundle_alias`.
pub const FIND_BY_NAME_SQL: &str =
    "SELECT b.id, b.name, b.set_hash, b.created_at, b.last_used_at \
     FROM __cas_bundle b \
     LEFT JOIN __cas_bundle_alias a ON a.bundle_id = b.id \
     WHERE b.name = ?1 OR a.name = ?1 \
     LIMIT 1";

/// `?1` = set_hash (TEXT). Returns 0-or-1 row of the bundle
/// summary columns.
pub const FIND_FIRST_BY_HASH_SQL: &str =
    "SELECT id, name, set_hash, created_at, last_used_at \
     FROM __cas_bundle WHERE set_hash = ?1 LIMIT 1";

/// `?1` = hex prefix pattern (TEXT, e.g. `"abcd%"`). Caller
/// validates `?1` is `[0-9a-f]+%` to prevent LIKE-wildcard
/// abuse. Returns N rows of the bundle summary columns.
pub const FIND_BY_HASH_PREFIX_SQL: &str =
    "SELECT id, name, set_hash, created_at, last_used_at \
     FROM __cas_bundle WHERE set_hash LIKE ?1 ORDER BY last_used_at DESC";

/// No params. Returns N rows of the bundle summary columns,
/// most-recently-used first.
pub const LIST_SQL: &str =
    "SELECT id, name, set_hash, created_at, last_used_at \
     FROM __cas_bundle ORDER BY last_used_at DESC";

/// `?1` = bundle_id (INTEGER). Returns 0-or-1 row of summary
/// columns. Use with `MEMBERS_SQL` + `BINARIES_SQL` for the
/// full detail shape.
pub const SHOW_SUMMARY_SQL: &str =
    "SELECT id, name, set_hash, created_at, last_used_at \
     FROM __cas_bundle WHERE id = ?1";

/// `?1` = bundle_id (INTEGER). Returns N rows of
/// `(extension_name TEXT, content_hash TEXT)`.
pub const MEMBERS_SQL: &str =
    "SELECT extension_name, content_hash \
     FROM __cas_bundle_member WHERE bundle_id = ?1 ORDER BY extension_name";

/// `?1` = bundle_id (INTEGER). Returns N rows of
/// `(target_triple TEXT, binary_path TEXT, built_at INTEGER)`.
pub const BINARIES_SQL: &str =
    "SELECT target_triple, binary_path, built_at \
     FROM __cas_bundle_binary WHERE bundle_id = ?1 ORDER BY target_triple";

/// `?1` = bundle_id (INTEGER). Removes the bundle row; cascade
/// drops the members + binaries + aliases via FK ON DELETE.
/// `changes` is 1 iff the bundle existed.
pub const DELETE_SQL: &str =
    "DELETE FROM __cas_bundle WHERE id = ?1";

/// `?1` = bundle_id (INTEGER), `?2` = now (INTEGER, unix
/// seconds). Bumps `last_used_at` for LRU eviction ordering.
pub const TOUCH_SQL: &str =
    "UPDATE __cas_bundle SET last_used_at = ?2 WHERE id = ?1";

/// `?1` = bundle_id (INTEGER). Returns 1 row with a single
/// INTEGER column: the number of members on this bundle.
pub const COUNT_MEMBERS_SQL: &str =
    "SELECT COUNT(*) FROM __cas_bundle_member WHERE bundle_id = ?1";

/// `?1` = bundle_id (INTEGER). Returns 1 row with a single
/// INTEGER column: the number of binaries on this bundle.
pub const COUNT_BINARIES_SQL: &str =
    "SELECT COUNT(*) FROM __cas_bundle_binary WHERE bundle_id = ?1";
