//! Content-addressed cache backed by SQLite. See
//! `PLAN-cas-cache.md` for the design and the crate-level
//! `Cargo.toml` description for the executive summary.

pub mod bundles;
pub mod resolver;
mod schema;
pub mod store;

pub use bundles::{
    BundleAliasConflict, BundleBinary, BundleDetail, BundleGcPolicy, BundleMember, BundleSummary,
};
#[cfg(feature = "https")]
pub use resolver::HttpsResolver;
pub use resolver::{ArtifactRef, ArtifactResolver, LocalFileResolver, ResolverRegistry, Source};
pub use store::{Hash, MergeStats, SqliteCasStore, StoreConfig, StoreMode, UriEntry};
