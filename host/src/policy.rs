//! In-WASM host policy plumbing.
//!
//! Re-exports the canonical `Policy` / `Capability` / `HttpPolicy` /
//! `PolicyError` from `sqlite-extension-policy`. The host's
//! crate-local `from_wit` conversion lives in `lib.rs` because the
//! source type is the bindgen-generated `LoadOptions` from the
//! `extension-loader-host` world — coupling the conversion to the
//! lib.rs bindgen invocation site keeps the dependency graph
//! simpler.

pub use sqlite_extension_policy::{Capability, DnsPolicy, HttpPolicy, Policy, PolicyError};
