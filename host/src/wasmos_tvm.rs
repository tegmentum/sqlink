//! ADR-0029 Phase 6.9 D2 Session 15a — sqlink-local wasmos install
//! path for the `tvm:memory@0.1.0` interfaces.
//!
//! Mirror of `tvm_wasmtime::wasmos_bindings` (which lives on
//! tvm-wasm's current main against wasmtime 48). Sqlink pins the
//! older tvm-wasm rev `6f3bae38` (wasmtime 46) for its
//! component-model-async requirement, so we can't consume the
//! upstream wasmos_bindings module directly — cross-major cargo
//! resolution rejects tvm-wasm's own wasmos deps at that rev
//! boundary.
//!
//! This local module is the sqlink-specific replacement for
//! `tvm_wasmtime::add_to_linker(&mut linker)?`. It exposes:
//!
//! - Mirror types (`Handle`, `RegionKind`, `Residency`, `RegionInfo`,
//!   `RegionMetrics`, `CompactResult`, `TvmError`) with wasmos
//!   `WitEnum` / `WitRecord` / `WitVariant` derives.
//! - Bidirectional `From` converters between the mirrors and the
//!   `tvm_wasmtime::bindings::tvm::memory::types::*` shapes.
//! - Three `#[host_iface(sync)]` structs
//!   (`TvmManagerHost` / `TvmBytesHost` / `TvmDiagnosticsHost`),
//!   each generic over `T: AsMut<TvmHost> + 'static`. Handlers
//!   pull `&mut TvmHost` via `ctx.consumer_state::<T>().as_mut()`
//!   and delegate to the wit-bindgen `Host` trait impls on
//!   `TvmHost` (which live in tvm-wasm and haven't changed).
//! - `install_tvm_memory_imports<T>(engine, linker, component)`
//!   composite — the drop-in for `tvm_wasmtime::add_to_linker`
//!   using the wasmos v46 async_bridge.
//!
//! When tvm-wasm and sqlink align on a common wasmtime version
//! (or when wasmos exposes an adapter-independent bindings crate),
//! this module retires in favor of the upstream one.
//!
//! # Session 15a → post-Phase-6.7 shape (2026-09-01)
//!
//! The original 15a version had to use a fn-pointer extractor
//! field on each host struct because wasmos's `#[host_iface]`
//! MVP rejected generic impl blocks. Phase 6.7 (wasmos
//! `0b55b5fd`) lifted that restriction; this module now uses
//! the natural `impl<T: AsMut<TvmHost> + Send + 'static>
//! TvmManagerHost<T>` shape — the fn-pointer + `TvmHostExtractor`
//! type alias are gone. `extract_tvm_host::<T>(ctx)` is the
//! shared helper that all three host struct method bodies use
//! to pull the projected `&mut TvmHost`.

use tvm_wasmtime::bindings::tvm::memory::bytes::Host as BgBytesHost;
use tvm_wasmtime::bindings::tvm::memory::diagnostics::Host as BgDiagnosticsHost;
use tvm_wasmtime::bindings::tvm::memory::manager::Host as BgManagerHost;
use tvm_wasmtime::bindings::tvm::memory::types as bg;
use tvm_wasmtime::TvmHost;
use wasmos_runtime_api::{
    host_iface, HostCallContext, HostImports, RuntimeError, RuntimeResult, WitEnum,
    WitRecord, WitVariant,
};
use wasmos_runtime_wasmtime_v48::async_bridge;
use wasmtime::component::{Component, Linker};
use wasmtime::Engine;

// ── Mirror types ─────────────────────────────────────────────────────

/// Sqlink-local mirror of [`bg::RegionKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, WitEnum)]
pub enum RegionKind {
    HotHeap,
    ObjectArena,
    BlobArena,
    PageStore,
    Scratch,
    DeviceState,
    CodeCache,
}

impl From<bg::RegionKind> for RegionKind {
    fn from(k: bg::RegionKind) -> Self {
        match k {
            bg::RegionKind::HotHeap => Self::HotHeap,
            bg::RegionKind::ObjectArena => Self::ObjectArena,
            bg::RegionKind::BlobArena => Self::BlobArena,
            bg::RegionKind::PageStore => Self::PageStore,
            bg::RegionKind::Scratch => Self::Scratch,
            bg::RegionKind::DeviceState => Self::DeviceState,
            bg::RegionKind::CodeCache => Self::CodeCache,
        }
    }
}
impl From<RegionKind> for bg::RegionKind {
    fn from(k: RegionKind) -> Self {
        match k {
            RegionKind::HotHeap => Self::HotHeap,
            RegionKind::ObjectArena => Self::ObjectArena,
            RegionKind::BlobArena => Self::BlobArena,
            RegionKind::PageStore => Self::PageStore,
            RegionKind::Scratch => Self::Scratch,
            RegionKind::DeviceState => Self::DeviceState,
            RegionKind::CodeCache => Self::CodeCache,
        }
    }
}

/// Sqlink-local mirror of [`bg::Residency`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, WitEnum)]
pub enum Residency {
    Hot,
    Warm,
    Cold,
    External,
}
impl From<bg::Residency> for Residency {
    fn from(r: bg::Residency) -> Self {
        match r {
            bg::Residency::Hot => Self::Hot,
            bg::Residency::Warm => Self::Warm,
            bg::Residency::Cold => Self::Cold,
            bg::Residency::External => Self::External,
        }
    }
}
impl From<Residency> for bg::Residency {
    fn from(r: Residency) -> Self {
        match r {
            Residency::Hot => Self::Hot,
            Residency::Warm => Self::Warm,
            Residency::Cold => Self::Cold,
            Residency::External => Self::External,
        }
    }
}

/// Sqlink-local mirror of [`bg::Handle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, WitRecord)]
pub struct Handle {
    pub region_id: u16,
    pub generation: u16,
    pub offset: u32,
}
impl From<bg::Handle> for Handle {
    fn from(h: bg::Handle) -> Self {
        Self { region_id: h.region_id, generation: h.generation, offset: h.offset }
    }
}
impl From<Handle> for bg::Handle {
    fn from(h: Handle) -> Self {
        Self { region_id: h.region_id, generation: h.generation, offset: h.offset }
    }
}

/// Sqlink-local mirror of [`bg::RegionInfo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, WitRecord)]
pub struct RegionInfo {
    pub id: u16,
    pub generation: u16,
    pub kind: RegionKind,
    pub capacity: u32,
    pub used: u32,
    pub residency: Residency,
}
impl From<bg::RegionInfo> for RegionInfo {
    fn from(r: bg::RegionInfo) -> Self {
        Self {
            id: r.id,
            generation: r.generation,
            kind: r.kind.into(),
            capacity: r.capacity,
            used: r.used,
            residency: r.residency.into(),
        }
    }
}
impl From<RegionInfo> for bg::RegionInfo {
    fn from(r: RegionInfo) -> Self {
        Self {
            id: r.id,
            generation: r.generation,
            kind: r.kind.into(),
            capacity: r.capacity,
            used: r.used,
            residency: r.residency.into(),
        }
    }
}

/// Sqlink-local mirror of [`bg::RegionMetrics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, WitRecord)]
pub struct RegionMetrics {
    pub allocations: u64,
    pub bytes_allocated: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub faults: u64,
    pub promotions: u64,
    pub demotions: u64,
}
impl From<bg::RegionMetrics> for RegionMetrics {
    fn from(m: bg::RegionMetrics) -> Self {
        Self {
            allocations: m.allocations,
            bytes_allocated: m.bytes_allocated,
            bytes_read: m.bytes_read,
            bytes_written: m.bytes_written,
            faults: m.faults,
            promotions: m.promotions,
            demotions: m.demotions,
        }
    }
}
impl From<RegionMetrics> for bg::RegionMetrics {
    fn from(m: RegionMetrics) -> Self {
        Self {
            allocations: m.allocations,
            bytes_allocated: m.bytes_allocated,
            bytes_read: m.bytes_read,
            bytes_written: m.bytes_written,
            faults: m.faults,
            promotions: m.promotions,
            demotions: m.demotions,
        }
    }
}

/// Sqlink-local mirror of [`bg::CompactResult`].
#[derive(Clone, Debug, PartialEq, Eq, WitRecord)]
pub struct CompactResult {
    pub old_generation: u16,
    pub new_generation: u16,
    pub mapping: Vec<(u32, u32)>,
}
impl From<bg::CompactResult> for CompactResult {
    fn from(c: bg::CompactResult) -> Self {
        Self {
            old_generation: c.old_generation,
            new_generation: c.new_generation,
            mapping: c.mapping,
        }
    }
}
impl From<CompactResult> for bg::CompactResult {
    fn from(c: CompactResult) -> Self {
        Self {
            old_generation: c.old_generation,
            new_generation: c.new_generation,
            mapping: c.mapping,
        }
    }
}

/// Sqlink-local mirror of [`bg::TvmError`].
#[derive(Clone, Debug, PartialEq, Eq, WitVariant)]
pub enum TvmError {
    RegionNotFound(u16),
    StaleHandle,
    OutOfBounds,
    NotResident,
    AllocationFailed,
    BackingStore(String),
    Pinned,
}
impl From<bg::TvmError> for TvmError {
    fn from(e: bg::TvmError) -> Self {
        match e {
            bg::TvmError::RegionNotFound(id) => Self::RegionNotFound(id),
            bg::TvmError::StaleHandle => Self::StaleHandle,
            bg::TvmError::OutOfBounds => Self::OutOfBounds,
            bg::TvmError::NotResident => Self::NotResident,
            bg::TvmError::AllocationFailed => Self::AllocationFailed,
            bg::TvmError::BackingStore(s) => Self::BackingStore(s),
            bg::TvmError::Pinned => Self::Pinned,
        }
    }
}
impl From<TvmError> for bg::TvmError {
    fn from(e: TvmError) -> Self {
        match e {
            TvmError::RegionNotFound(id) => Self::RegionNotFound(id),
            TvmError::StaleHandle => Self::StaleHandle,
            TvmError::OutOfBounds => Self::OutOfBounds,
            TvmError::NotResident => Self::NotResident,
            TvmError::AllocationFailed => Self::AllocationFailed,
            TvmError::BackingStore(s) => Self::BackingStore(s),
            TvmError::Pinned => Self::Pinned,
        }
    }
}

// ── Host-state extractor ────────────────────────────────────────────
//
// Phase 6.7 (wasmos `0b55b5fd`) lifted the "no generic impl blocks"
// restriction on `#[host_iface]`. The 15a fn-pointer-extractor
// workaround is gone; the host structs below use the natural
// `impl<T: AsMut<TvmHost> + Send + 'static> TvmManagerHost<T>`
// shape and pull `&mut TvmHost` via `ctx.consumer_state::<T>()`
// inline at every method body.

fn extract_tvm_host<'a, T: AsMut<TvmHost> + 'static>(
    ctx: &'a mut HostCallContext<'_>,
) -> RuntimeResult<&'a mut TvmHost> {
    let state = ctx.consumer_state::<T>().ok_or_else(|| {
        RuntimeError::msg(format!(
            "sqlink::wasmos_tvm: no `{}` in ctx.consumer_state — the wasmos v46 async_bridge \
             must be installed against a wasmtime::Store<T> where T: AsMut<TvmHost>",
            std::any::type_name::<T>(),
        ))
    })?;
    Ok(state.as_mut())
}

// ── Manager interface ────────────────────────────────────────────────

/// Wasmos-native implementation of `tvm:memory/manager@0.1.0`.
/// Generic over the consumer state type `T: AsMut<TvmHost>` (sqlink's
/// `RunState`, `CliRunState`, or `State` — all three impl it).
/// Handlers pull `&mut TvmHost` via `ctx.consumer_state::<T>()`
/// inline; PhantomData carries the T-monomorphisation without
/// storing a T value at runtime.
pub struct TvmManagerHost<T> {
    _marker: std::marker::PhantomData<fn() -> T>,
}
impl<T> TvmManagerHost<T> {
    pub const fn new() -> Self {
        Self { _marker: std::marker::PhantomData }
    }
}
impl<T> Default for TvmManagerHost<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[host_iface(sync)]
impl<T: AsMut<TvmHost> + Send + 'static> TvmManagerHost<T> {
    fn create_region(
        &self,
        ctx: &mut HostCallContext<'_>,
        kind: RegionKind,
        capacity: u32,
    ) -> RuntimeResult<Result<u16, TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::create_region(host, kind.into(), capacity).map_err(Into::into))
    }

    fn destroy_region(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::destroy_region(host, region_id).map_err(Into::into))
    }

    fn alloc(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
        size: u32,
    ) -> RuntimeResult<Result<Handle, TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::alloc(host, region_id, size)
            .map(Into::into)
            .map_err(Into::into))
    }

    fn dealloc(
        &self,
        ctx: &mut HostCallContext<'_>,
        ptr: Handle,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::dealloc(host, ptr.into()).map_err(Into::into))
    }

    fn describe_region(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<RegionInfo, TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::describe_region(host, region_id)
            .map(Into::into)
            .map_err(Into::into))
    }

    fn promote_region(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::promote_region(host, region_id).map_err(Into::into))
    }

    fn demote_region(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::demote_region(host, region_id).map_err(Into::into))
    }

    fn spill_region(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::spill_region(host, region_id).map_err(Into::into))
    }

    fn load_region(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::load_region(host, region_id).map_err(Into::into))
    }

    fn pin(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::pin(host, region_id).map_err(Into::into))
    }

    fn unpin(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::unpin(host, region_id).map_err(Into::into))
    }

    fn compact_region(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<CompactResult, TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgManagerHost::compact_region(host, region_id)
            .map(Into::into)
            .map_err(Into::into))
    }
}

// ── Bytes interface ─────────────────────────────────────────────────

/// Wasmos-native implementation of `tvm:memory/bytes@0.1.0`.
pub struct TvmBytesHost<T> {
    _marker: std::marker::PhantomData<fn() -> T>,
}
impl<T> TvmBytesHost<T> {
    pub const fn new() -> Self {
        Self { _marker: std::marker::PhantomData }
    }
}
impl<T> Default for TvmBytesHost<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[host_iface(sync)]
impl<T: AsMut<TvmHost> + Send + 'static> TvmBytesHost<T> {
    fn read(
        &self,
        ctx: &mut HostCallContext<'_>,
        ptr: Handle,
        len: u32,
    ) -> RuntimeResult<Result<Vec<u8>, TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgBytesHost::read(host, ptr.into(), len).map_err(Into::into))
    }

    fn write(
        &self,
        ctx: &mut HostCallContext<'_>,
        ptr: Handle,
        data: Vec<u8>,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgBytesHost::write(host, ptr.into(), data).map_err(Into::into))
    }

    fn copy(
        &self,
        ctx: &mut HostCallContext<'_>,
        src: Handle,
        dst: Handle,
        len: u32,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgBytesHost::copy(host, src.into(), dst.into(), len).map_err(Into::into))
    }

    fn read_into(
        &self,
        ctx: &mut HostCallContext<'_>,
        src: Handle,
        dst_region: u16,
        dst_offset: u32,
        len: u32,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgBytesHost::read_into(host, src.into(), dst_region, dst_offset, len)
            .map_err(Into::into))
    }

    fn write_from(
        &self,
        ctx: &mut HostCallContext<'_>,
        src_region: u16,
        src_offset: u32,
        dst: Handle,
        len: u32,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgBytesHost::write_from(host, src_region, src_offset, dst.into(), len)
            .map_err(Into::into))
    }

    fn copy_region(
        &self,
        ctx: &mut HostCallContext<'_>,
        src_region: u16,
        src_offset: u32,
        dst_region: u16,
        dst_offset: u32,
        len: u32,
    ) -> RuntimeResult<Result<(), TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgBytesHost::copy_region(host, src_region, src_offset, dst_region, dst_offset, len)
            .map_err(Into::into))
    }
}

// ── Diagnostics interface ───────────────────────────────────────────

/// Wasmos-native implementation of `tvm:memory/diagnostics@0.1.0`.
pub struct TvmDiagnosticsHost<T> {
    _marker: std::marker::PhantomData<fn() -> T>,
}
impl<T> TvmDiagnosticsHost<T> {
    pub const fn new() -> Self {
        Self { _marker: std::marker::PhantomData }
    }
}
impl<T> Default for TvmDiagnosticsHost<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[host_iface(sync)]
impl<T: AsMut<TvmHost> + Send + 'static> TvmDiagnosticsHost<T> {
    fn list_regions(
        &self,
        ctx: &mut HostCallContext<'_>,
    ) -> RuntimeResult<Vec<RegionInfo>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgDiagnosticsHost::list_regions(host)
            .into_iter()
            .map(Into::into)
            .collect())
    }

    fn fault_count(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<u64> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgDiagnosticsHost::fault_count(host, region_id))
    }

    fn allocation_count(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<u64> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgDiagnosticsHost::allocation_count(host, region_id))
    }

    fn bytes_read_count(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<u64> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgDiagnosticsHost::bytes_read_count(host, region_id))
    }

    fn bytes_written_count(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<u64> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgDiagnosticsHost::bytes_written_count(host, region_id))
    }

    fn metrics_snapshot(
        &self,
        ctx: &mut HostCallContext<'_>,
        region_id: u16,
    ) -> RuntimeResult<Result<RegionMetrics, TvmError>> {
        let host = extract_tvm_host::<T>(ctx)?;
        Ok(BgDiagnosticsHost::metrics_snapshot(host, region_id)
            .map(Into::into)
            .map_err(Into::into))
    }
}

// ── Install fn ─────────────────────────────────────────────────────

/// Sqlink-local drop-in for `tvm_wasmtime::add_to_linker`. Wires
/// all three `tvm:memory@0.1.0` interfaces (manager / bytes /
/// diagnostics) into the caller's `wasmtime::component::Linker<T>`
/// via the wasmos v46 async_bridge. Handlers reach `&mut TvmHost`
/// via `ctx.consumer_state::<T>().as_mut()`.
///
/// Consumer requirement: `T` (the store data type) impls
/// `AsMut<TvmHost>`. Both sqlink's `RunState` and `CliRunState`
/// satisfy that.
///
/// Additive to the wit-bindgen `add_to_linker` — either can be
/// wired against the same linker + component pair; a component
/// only binds the imports it declares. Sqlink migrates one call
/// site at a time.
pub fn install_tvm_memory_imports<T>(
    engine: &Engine,
    linker: &mut Linker<T>,
    component: &Component,
) -> anyhow::Result<()>
where
    T: AsMut<TvmHost> + Send + 'static,
{
    use anyhow::anyhow;

    let imports = HostImports::new()
        .register_sync("tvm:memory/manager@0.1.0", TvmManagerHost::<T>::new())
        .register_sync("tvm:memory/bytes@0.1.0", TvmBytesHost::<T>::new())
        .register_sync(
            "tvm:memory/diagnostics@0.1.0",
            TvmDiagnosticsHost::<T>::new(),
        );

    async_bridge::install_host_imports(engine, linker, component, &imports)
        .map_err(|e| anyhow!("wasmos_tvm install_host_imports: {e}"))
}
