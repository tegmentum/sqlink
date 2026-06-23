//! Register sqlite3 C trampolines on the user-process db handle.
//!
//! Each loaded extension's scalar / aggregate functions become a
//! sqlite3 function on the host process's db, routing through the
//! captured pApi `create_function_v2` / `create_window_function`.
//! The C callbacks marshal args  call `sqlink-host` async dispatch
//! synchronously  marshal result back.
//!
//! Mirror of `register_host_loaded_scalar` and the
//! `HostLoadedAggregate` trait impl in `sqlink-host::lib.rs`, but
//! every sqlite3_* C call goes through the pApi table instead of
//! libsqlite3-sys's bundled symbols.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sqlink_host::bindings::sqlite::extension::types::SqlValue;
use sqlink_host::Host;
use tokio::runtime::Runtime;

use crate::api::{
    sqlite3, sqlite3_context, sqlite3_value, ApiRoutines, SQLITE_DETERMINISTIC,
    SQLITE_OK, SQLITE_UTF8,
};
use crate::value::{read_value, write_error, write_result};

// ─── Scalar trampoline ───────────────────────────────────────────

struct ScalarCtx {
    host: Host,
    rt: Arc<Runtime>,
    ext_name: String,
    func_id: u64,
    api: ApiRoutines,
}

unsafe extern "C" fn scalar_xfunc(
    ctx: *mut sqlite3_context,
    argc: c_int,
    argv: *mut *mut sqlite3_value,
) {
    let api_routines = match get_api_for_ctx(ctx) {
        Some(a) => a,
        None => return,
    };
    let user_data_fn = api_routines.as_ref().user_data.expect("user_data");
    let raw = user_data_fn(ctx) as *const ScalarCtx;
    if raw.is_null() {
        write_error(&api_routines, ctx, "sqlink-loader scalar: null trampoline ctx");
        return;
    }
    let scalar_ctx: &ScalarCtx = &*raw;

    let mut args: Vec<SqlValue> = Vec::with_capacity(argc as usize);
    for i in 0..argc {
        let v = *argv.add(i as usize);
        args.push(read_value(&scalar_ctx.api, v));
    }

    // block_on  the host's dispatch_scalar is async. We can't use
    // block_in_place + Handle::current because we're called from a
    // thread sqlite3 owns (no surrounding tokio task). The
    // Runtime::block_on path is the canonical way to bridge.
    let dispatch = scalar_ctx
        .rt
        .block_on(scalar_ctx.host.dispatch_scalar(
            &scalar_ctx.ext_name,
            scalar_ctx.func_id,
            args,
        ));
    match dispatch {
        Ok(Ok(v)) => write_result(&scalar_ctx.api, ctx, v),
        Ok(Err(extension_err)) => {
            write_error(&scalar_ctx.api, ctx, &extension_err);
        }
        Err(host_err) => {
            let msg = format!("sqlink-loader dispatch_scalar: {host_err}");
            write_error(&scalar_ctx.api, ctx, &msg);
        }
    }
}

unsafe extern "C" fn scalar_destructor(p: *mut c_void) {
    if !p.is_null() {
        drop(Box::from_raw(p as *mut ScalarCtx));
    }
}

/// Register one wasm-extension scalar on the user-process db via
/// pApi `create_function_v2`. Returns sqlite3 result code; 0 on
/// success.
pub unsafe fn register_scalar(
    api: ApiRoutines,
    db: *mut sqlite3,
    host: Host,
    rt: Arc<Runtime>,
    ext_name: &str,
    func_name: &str,
    num_args: i32,
    func_id: u64,
) -> c_int {
    let boxed = Box::new(ScalarCtx {
        host,
        rt,
        ext_name: ext_name.to_string(),
        func_id,
        api,
    });
    let ptr = Box::into_raw(boxed) as *mut c_void;

    let name_c = match CString::new(func_name) {
        Ok(c) => c,
        Err(_) => {
            drop(Box::from_raw(ptr as *mut ScalarCtx));
            return crate::api::SQLITE_MISUSE;
        }
    };

    let create = api.as_ref().create_function_v2.expect("create_function_v2");
    create(
        db,
        name_c.as_ptr() as *const c_char,
        num_args as c_int,
        SQLITE_UTF8 | SQLITE_DETERMINISTIC,
        ptr,
        Some(scalar_xfunc),
        None,
        None,
        Some(scalar_destructor),
    )
}

// ─── Aggregate trampoline ────────────────────────────────────────
//
// Mirrors HostLoadedAggregate in host/src/lib.rs. State per
// in-flight aggregate is keyed by a u64 context-id that the host
// allocates monotonically; the wasm side stores the actual
// accumulator. Our pApi aggregate_context call returns a sqlite3-
// managed buffer of size `sizeof(u64)`  on first xStep call for
// this row group, zero-filled. We treat that as "uninitialized"
// and stash a new context-id; subsequent xStep calls find it
// already set.

struct AggCtx {
    host: Host,
    rt: Arc<Runtime>,
    ext_name: String,
    func_id: u64,
    api: ApiRoutines,
    /// Monotonic counter for fresh aggregate context ids. We mirror
    /// the host's `agg_ctx_counter` here so each xStep without a
    /// preexisting wasm-side state gets a unique id. Atomically
    /// bumped on init only — once the sqlite-side
    /// `aggregate_context` returns the same buffer across xStep
    /// calls for one row group, we keep its stored id.
    next_id: AtomicU64,
}

/// Pull or initialize the per-aggregate-call context-id stashed
/// inside sqlite3's `aggregate_context` buffer. Returns (id,
/// first_call). Zero is a sentinel for "uninitialized" because
/// xinit is allocator-zeroed by sqlite3 contract.
unsafe fn agg_state(
    api: &ApiRoutines,
    sqlite_ctx: *mut sqlite3_context,
    agg: &AggCtx,
) -> Option<(u64, bool)> {
    let aggctx_fn = api.as_ref().aggregate_context.expect("aggregate_context");
    let buf = aggctx_fn(sqlite_ctx, std::mem::size_of::<u64>() as c_int);
    if buf.is_null() {
        return None;
    }
    let slot = buf as *mut u64;
    let cur = *slot;
    if cur == 0 {
        // Fresh row group. Allocate a new context-id, starting from
        // 1 so 0 stays reserved for the uninit sentinel.
        let id = agg.next_id.fetch_add(1, Ordering::Relaxed);
        let id = if id == 0 { agg.next_id.fetch_add(1, Ordering::Relaxed) } else { id };
        *slot = id;
        Some((id, true))
    } else {
        Some((cur, false))
    }
}

unsafe extern "C" fn agg_xstep(
    ctx: *mut sqlite3_context,
    argc: c_int,
    argv: *mut *mut sqlite3_value,
) {
    let api_routines = match get_api_for_ctx(ctx) {
        Some(a) => a,
        None => return,
    };
    let user_data_fn = api_routines.as_ref().user_data.expect("user_data");
    let raw = user_data_fn(ctx) as *const AggCtx;
    if raw.is_null() {
        write_error(&api_routines, ctx, "sqlink-loader aggregate: null trampoline ctx");
        return;
    }
    let agg: &AggCtx = &*raw;
    let (context_id, _first) = match agg_state(&agg.api, ctx, agg) {
        Some(s) => s,
        None => {
            write_error(&agg.api, ctx, "sqlink-loader aggregate: out of memory");
            return;
        }
    };
    let mut args: Vec<SqlValue> = Vec::with_capacity(argc as usize);
    for i in 0..argc {
        let v = *argv.add(i as usize);
        args.push(read_value(&agg.api, v));
    }
    let result = agg.rt.block_on(agg.host.dispatch_aggregate_step(
        &agg.ext_name,
        agg.func_id,
        context_id,
        args,
    ));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(extension_err)) => write_error(&agg.api, ctx, &extension_err),
        Err(host_err) => {
            write_error(
                &agg.api,
                ctx,
                &format!("sqlink-loader dispatch_aggregate_step: {host_err}"),
            );
        }
    }
}

unsafe extern "C" fn agg_xfinal(ctx: *mut sqlite3_context) {
    let api_routines = match get_api_for_ctx(ctx) {
        Some(a) => a,
        None => return,
    };
    let user_data_fn = api_routines.as_ref().user_data.expect("user_data");
    let raw = user_data_fn(ctx) as *const AggCtx;
    if raw.is_null() {
        write_error(&api_routines, ctx, "sqlink-loader aggregate: null trampoline ctx");
        return;
    }
    let agg: &AggCtx = &*raw;
    // aggregate_context with size 0 returns the existing buffer or
    // NULL if xStep was never called on this row group. If NULL,
    // emit NULL result (matches sqlite3's behavior for empty agg).
    let aggctx_fn = agg.api.as_ref().aggregate_context.expect("aggregate_context");
    let buf = aggctx_fn(ctx, 0);
    let context_id = if buf.is_null() {
        0
    } else {
        let slot = buf as *const u64;
        *slot
    };
    let result = agg.rt.block_on(agg.host.dispatch_aggregate_finalize(
        &agg.ext_name,
        agg.func_id,
        context_id,
    ));
    match result {
        Ok(Ok(v)) => write_result(&agg.api, ctx, v),
        Ok(Err(extension_err)) => write_error(&agg.api, ctx, &extension_err),
        Err(host_err) => write_error(
            &agg.api,
            ctx,
            &format!("sqlink-loader dispatch_aggregate_finalize: {host_err}"),
        ),
    }
}

unsafe extern "C" fn agg_xvalue(ctx: *mut sqlite3_context) {
    // Window-function path: produce the current intermediate value
    // WITHOUT releasing the context.
    let api_routines = match get_api_for_ctx(ctx) {
        Some(a) => a,
        None => return,
    };
    let user_data_fn = api_routines.as_ref().user_data.expect("user_data");
    let raw = user_data_fn(ctx) as *const AggCtx;
    if raw.is_null() {
        write_error(&api_routines, ctx, "sqlink-loader aggregate: null trampoline ctx");
        return;
    }
    let agg: &AggCtx = &*raw;
    let aggctx_fn = agg.api.as_ref().aggregate_context.expect("aggregate_context");
    let buf = aggctx_fn(ctx, 0);
    let context_id = if buf.is_null() {
        0
    } else {
        *(buf as *const u64)
    };
    let result = agg.rt.block_on(agg.host.dispatch_aggregate_value(
        &agg.ext_name,
        agg.func_id,
        context_id,
    ));
    match result {
        Ok(Ok(v)) => write_result(&agg.api, ctx, v),
        Ok(Err(extension_err)) => write_error(&agg.api, ctx, &extension_err),
        Err(host_err) => write_error(
            &agg.api,
            ctx,
            &format!("sqlink-loader dispatch_aggregate_value: {host_err}"),
        ),
    }
}

unsafe extern "C" fn agg_xinverse(
    ctx: *mut sqlite3_context,
    argc: c_int,
    argv: *mut *mut sqlite3_value,
) {
    let api_routines = match get_api_for_ctx(ctx) {
        Some(a) => a,
        None => return,
    };
    let user_data_fn = api_routines.as_ref().user_data.expect("user_data");
    let raw = user_data_fn(ctx) as *const AggCtx;
    if raw.is_null() {
        write_error(&api_routines, ctx, "sqlink-loader aggregate: null trampoline ctx");
        return;
    }
    let agg: &AggCtx = &*raw;
    let aggctx_fn = agg.api.as_ref().aggregate_context.expect("aggregate_context");
    let buf = aggctx_fn(ctx, 0);
    let context_id = if buf.is_null() {
        0
    } else {
        *(buf as *const u64)
    };
    let mut args: Vec<SqlValue> = Vec::with_capacity(argc as usize);
    for i in 0..argc {
        let v = *argv.add(i as usize);
        args.push(read_value(&agg.api, v));
    }
    let result = agg.rt.block_on(agg.host.dispatch_aggregate_inverse(
        &agg.ext_name,
        agg.func_id,
        context_id,
        args,
    ));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(extension_err)) => write_error(&agg.api, ctx, &extension_err),
        Err(host_err) => write_error(
            &agg.api,
            ctx,
            &format!("sqlink-loader dispatch_aggregate_inverse: {host_err}"),
        ),
    }
}

unsafe extern "C" fn agg_destructor(p: *mut c_void) {
    if !p.is_null() {
        drop(Box::from_raw(p as *mut AggCtx));
    }
}

/// Register one wasm-extension aggregate (or window-aggregate) on
/// the user-process db via pApi `create_function_v2` (or
/// `create_window_function`). Returns sqlite3 result code.
pub unsafe fn register_aggregate(
    api: ApiRoutines,
    db: *mut sqlite3,
    host: Host,
    rt: Arc<Runtime>,
    ext_name: &str,
    func_name: &str,
    num_args: i32,
    func_id: u64,
    is_window: bool,
) -> c_int {
    let boxed = Box::new(AggCtx {
        host,
        rt,
        ext_name: ext_name.to_string(),
        func_id,
        api,
        next_id: AtomicU64::new(1),
    });
    let ptr = Box::into_raw(boxed) as *mut c_void;

    let name_c = match CString::new(func_name) {
        Ok(c) => c,
        Err(_) => {
            drop(Box::from_raw(ptr as *mut AggCtx));
            return crate::api::SQLITE_MISUSE;
        }
    };

    if is_window {
        let create = api
            .as_ref()
            .create_window_function
            .expect("create_window_function");
        create(
            db,
            name_c.as_ptr() as *const c_char,
            num_args as c_int,
            SQLITE_UTF8,
            ptr,
            Some(agg_xstep),
            Some(agg_xfinal),
            Some(agg_xvalue),
            Some(agg_xinverse),
            Some(agg_destructor),
        )
    } else {
        let create = api.as_ref().create_function_v2.expect("create_function_v2");
        create(
            db,
            name_c.as_ptr() as *const c_char,
            num_args as c_int,
            SQLITE_UTF8,
            ptr,
            None,
            Some(agg_xstep),
            Some(agg_xfinal),
            Some(agg_destructor),
        )
    }
}

/// Look up the captured pApi from the global stash. The trampolines
/// don't receive pApi directly  sqlite3 only passes a ctx pointer
/// and our user_data blob. We retrieve the table from the static.
fn get_api_for_ctx(_ctx: *mut sqlite3_context) -> Option<ApiRoutines> {
    crate::state::api_routines()
}

// Silence unused.
const _: c_int = SQLITE_OK;
