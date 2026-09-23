//! Hand-rolled `sqlite:extension/vtab@1.0.0` record + enum types.
//! Retires the last consumer of the `loaded_tabular` bindgen (dispatch
//! moved to `wasmos_mutating_dispatch` in Phase 2b, `b2dcd692`). These
//! types are the record/enum shapes returned by the vtab TypedFuncs
//! (`IndexPlan`, `VtabRow`, etc.) and consumed by
//! `dispatch_vtab_fetch_batch` / `convert_index_*_to_loaded_tabular`.
//!
//! Each `#[component(record|enum)]` derive matches the WIT signature
//! byte-for-byte. Field / variant name normalization: `snake_case`
//! Rust ↔ `kebab-case` WIT (wasmtime handles the split automatically);
//! Rust reserved words get an explicit `#[component(name = ...)]`.

use wasmtime::component::{ComponentType, Lift, Lower};

use crate::wasmos_extension_types::SqlValue;

#[derive(ComponentType, Lift, Lower, Copy, Clone, Debug, PartialEq, Eq)]
#[component(enum)]
#[repr(u8)]
pub enum ConstraintOp {
    #[component(name = "eq")]
    Eq,
    #[component(name = "gt")]
    Gt,
    #[component(name = "le")]
    Le,
    #[component(name = "lt")]
    Lt,
    #[component(name = "ge")]
    Ge,
    #[component(name = "ne")]
    Ne,
    #[component(name = "match")]
    Match,
    #[component(name = "like")]
    Like,
    #[component(name = "regexp")]
    Regexp,
    #[component(name = "glob")]
    Glob,
    #[component(name = "is-null")]
    IsNull,
    #[component(name = "is-not-null")]
    IsNotNull,
    #[component(name = "limit")]
    Limit,
    #[component(name = "offset")]
    Offset,
    #[component(name = "function")]
    Function,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct Constraint {
    pub column: i32,
    pub op: ConstraintOp,
    pub usable: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct Orderby {
    pub column: i32,
    pub desc: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct IndexInfo {
    pub constraints: Vec<Constraint>,
    pub orderbys: Vec<Orderby>,
    #[component(name = "col-used")]
    pub col_used: u64,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct ConstraintUsage {
    #[component(name = "argv-index")]
    pub argv_index: i32,
    pub omit: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct IndexPlan {
    #[component(name = "constraint-usage")]
    pub constraint_usage: Vec<ConstraintUsage>,
    #[component(name = "idx-num")]
    pub idx_num: i32,
    #[component(name = "idx-str")]
    pub idx_str: Option<String>,
    #[component(name = "estimated-cost")]
    pub estimated_cost: f64,
    #[component(name = "estimated-rows")]
    pub estimated_rows: i64,
    #[component(name = "orderby-consumed")]
    pub orderby_consumed: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct VtabRow {
    pub rowid: i64,
    pub columns: Vec<SqlValue>,
}
