//! Shared parsers extracted from sqlink consumers so the canonical
//! source lives in exactly one place (and the fuzz harnesses can
//! import it instead of carrying copy-with-sync-comment bodies).
//!
//! `duration` is no_std + alloc only so wasm extensions like
//! `bundle-cli` can use it directly. `load_args` is gated behind
//! the default `std` feature because it builds the
//! `sqlite-extension-policy::Policy` type and needs `anyhow`.

#![no_std]

extern crate alloc;

pub mod duration;

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "std")]
pub mod load_args;

#[cfg(feature = "std")]
pub mod spawn_build_validation;
