//! Embed path for dns. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.
//!
//! NOTE: dns's `dns_resolve` scalar relies on the host's
//! `sqlite::extension::dns::resolve` SPI, which is a wit-import only
//! available in the wasi component (`.load`) path. The embed path
//! runs inside the cli's own process and has no such capability hook,
//! so `dns_resolve` is stubbed to return an error directing callers
//! to use the `.load`-able component build. The surface (name,
//! arity, ScalarSpec) is preserved so a hosted-dns embed bridge can
//! be wired in later without touching call sites.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_RESOLVE: u64 = 1;

pub fn call_scalar(func_id: u64, _args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_RESOLVE => Err("dns_resolve: not available in embed build (host dns SPI \
             is wasi-component-only); use the `.load`-able \
             dns_extension.component.wasm with --grant=dns instead"
            .into()),
        other => Err(format!("dns: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[ScalarSpec {
    func_id: FID_RESOLVE,
    name: b"dns_resolve\0",
    num_args: 2,
    deterministic: false,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
