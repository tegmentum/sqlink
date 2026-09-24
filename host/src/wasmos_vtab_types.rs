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

use wasmos_runtime_api::{WitEnum, WitRecord};
use wasmtime::component::{ComponentType, Lift, Lower};

use crate::wasmos_extension_types::SqlValue;

// Bindgen `with:` remap scaffolding. `sqlite:extension/vtab@1.0.0`
// is types-only (no functions to implement — the vtab callback
// surface lives in `sqlite:extension/vtab-update`), so both trait
// definitions are empty markers and `add_to_linker` is a no-op.
// When the `bindings` bindgen `with:` clause remaps
// `sqlite:extension/vtab@1.0.0` onto this module, the macro-
// generated code inside the target world sees these stubs and
// compiles.
pub trait Host {}
impl<_T: Host + ?Sized> Host for &mut _T {}

pub trait HostWithStore<T>: wasmtime::component::HasData {}
impl<H: ?Sized, T> HostWithStore<T> for H where H: wasmtime::component::HasData {}

pub fn add_to_linker_instance<T, D>(
    _inst: &mut wasmtime::component::LinkerInstance<'_, T>,
    _host_getter: fn(&mut T) -> D::Data<'_>,
) -> wasmtime::Result<()>
where
    D: HostWithStore<T>,
    for<'a> D::Data<'a>: Host,
    T: 'static,
{
    Ok(())
}

pub fn add_to_linker<T, D>(
    linker: &mut wasmtime::component::Linker<T>,
    host_getter: fn(&mut T) -> D::Data<'_>,
) -> wasmtime::Result<()>
where
    D: HostWithStore<T>,
    for<'a> D::Data<'a>: Host,
    T: 'static,
{
    let mut inst = linker.instance("sqlite:extension/vtab@1.0.0")?;
    add_to_linker_instance::<T, D>(&mut inst, host_getter)
}

#[derive(ComponentType, Lift, Lower, Copy, Clone, Debug, PartialEq, Eq, WitEnum)]
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

#[derive(ComponentType, Lift, Lower, Clone, Debug, WitRecord)]
#[component(record)]
pub struct Constraint {
    pub column: i32,
    pub op: ConstraintOp,
    pub usable: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug, WitRecord)]
#[component(record)]
pub struct Orderby {
    pub column: i32,
    pub desc: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug, WitRecord)]
#[component(record)]
pub struct IndexInfo {
    pub constraints: Vec<Constraint>,
    pub orderbys: Vec<Orderby>,
    #[component(name = "col-used")]
    pub col_used: u64,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug, WitRecord)]
#[component(record)]
pub struct ConstraintUsage {
    #[component(name = "argv-index")]
    pub argv_index: i32,
    pub omit: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug, WitRecord)]
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

#[derive(ComponentType, Lift, Lower, Clone, Debug, WitRecord)]
#[component(record)]
pub struct VtabRow {
    pub rowid: i64,
    pub columns: Vec<SqlValue>,
}
