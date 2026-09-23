//! Phase 3 groundwork: hand-rolled `sqlite:extension` types with
//! `#[derive(ComponentType, Lift, Lower)]` and the
//! `wasmtime::component::flags!` macro. Consumers migrate off the
//! `loaded` bindgen block one interface at a time; when the last
//! consumer is retired, the bindgen block itself is deleted.
//!
//! Each record/variant/enum/flags declaration matches the WIT
//! signature byte-for-byte. Field / variant name normalization:
//! `snake_case` Rust ↔ `kebab-case` WIT (wasmtime handles the split
//! automatically); Rust reserved words get an explicit
//! `#[component(name = ...)]`.
//!
//! Sibling of [`crate::wasmos_vtab_types`], which owns the same
//! treatment for `sqlite:extension/vtab@1.0.0`.

use wasmtime::component::{flags, ComponentType, Lift, Lower};

// ────────────────────────────────────────────────────────────────────
// sqlite:extension/types@1.0.0
// ────────────────────────────────────────────────────────────────────

/// Mirrors the WIT `types.wit-value-payload` record. Carries a
/// canonical-CBOR-encoded WIT record across the host/extension
/// boundary.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct WitValuePayload {
    #[component(name = "type-id")]
    pub type_id: Vec<u8>,
    pub bytes: Vec<u8>,
    #[component(name = "symbolic-name")]
    pub symbolic_name: String,
}

/// Mirrors the WIT `types.sql-value` variant. The unified SQL value
/// representation used for arguments, results, parameter binding,
/// and row data.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(variant)]
pub enum SqlValue {
    #[component(name = "null")]
    Null,
    #[component(name = "integer")]
    Integer(i64),
    #[component(name = "real")]
    Real(f64),
    #[component(name = "text")]
    Text(String),
    #[component(name = "blob")]
    Blob(Vec<u8>),
    #[component(name = "wit-value")]
    WitValue(WitValuePayload),
}

/// Mirrors the WIT `types.sqlite-error` record.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct SqliteError {
    pub code: i32,
    #[component(name = "extended-code")]
    pub extended_code: i32,
    pub message: String,
}

// Mirrors the WIT `types.function-flags` flags type.
// `DETERMINISTIC = 1`, `DIRECT_ONLY = 2`, `INNOCUOUS = 4`.
flags! {
    FunctionFlags {
        #[component(name = "deterministic")]
        const DETERMINISTIC;
        #[component(name = "direct-only")]
        const DIRECT_ONLY;
        #[component(name = "innocuous")]
        const INNOCUOUS;
    }
}
