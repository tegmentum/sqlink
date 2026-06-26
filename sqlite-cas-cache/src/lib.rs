//! Content-addressed cache backed by SQLite. See
//! `PLAN-cas-cache.md` for the design and the crate-level
//! `Cargo.toml` description for the executive summary.

pub mod bundles;
// v1.5 round 2 (PLAN-followups.md): connection-driven bundle CRUD
// free functions + their SQL-string `pub const`s. The single
// source of truth for bundles SQL across (a) the rusqlite-ish
// SqliteCasStore wrapper in this crate (kept; other consumers
// depend on it), (b) the browser JS polyfill's inline mirrors
// (browser/src/extension-loader.js's buildBundlesPolyfill),
// (c) the native sqlink-host's `impl bundles::Host` cutover
// that uses this module's free functions directly against a
// `~/.cache/sqlink/cas.db` Connection without going through
// SqliteCasStore.
pub mod bundles_exec;
pub mod resolver;
// v1.5 (PLAN-followups.md): expose the schema DDL strings publicly
// so the browser bundles polyfill can run the same migration ladder
// against an OPFS/in-memory cas db reached through `sqlite-wasm`'s
// dispatch-bridge.bridged-execute-cas. The strings are pure `&str`
// constants (no runtime deps) so they're trivially reachable from a
// `no_std + alloc` consumer if a future native unify lands. Native
// (this crate's rusqlite path) keeps using them via `store.rs`.
pub mod schema;
pub mod store;

pub use bundles::{
    BundleAliasConflict, BundleBinary, BundleDetail, BundleGcPolicy, BundleMember, BundleSummary,
};
#[cfg(feature = "https")]
pub use resolver::HttpsResolver;
pub use resolver::{ArtifactRef, ArtifactResolver, LocalFileResolver, ResolverRegistry, Source};
pub use store::{Hash, MergeStats, SqliteCasStore, StoreConfig, StoreMode, UriEntry};
