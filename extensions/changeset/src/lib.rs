//! `changeset` extension — Phase 1 pure-function helpers for
//! SQLite session/changeset blobs. See Cargo.toml for the
//! contract.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;
