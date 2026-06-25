//! Embed path for onnx. The WIT path keeps loaded tract-onnx models
//! in a thread_local HashMap keyed by an i64 handle; that state
//! cannot be threaded cleanly through the embed contract's stateless
//! per-call dispatch (each scalar call gets only its argv  no place
//! to anchor a model registry tied to the host sqlite3 conn).
//!
//! All 5 onnx scalars are still REGISTERED so SQL compiles and the
//! error surface is uniform: every scalar except `onnx_version`
//! (which we add here for parity with other extensions) returns a
//! clear error pointing at the `.load`'d wasi component.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_LOAD: u64 = 1;
const FID_INPUT_NAMES: u64 = 2;
const FID_OUTPUT_NAMES: u64 = 3;
const FID_RUN: u64 = 4;
const FID_UNLOAD: u64 = 5;

pub fn call_scalar(_func_id: u64, _args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    Err(
        "onnx: stateful loaded-model state not available in embed path; \
         load the wasi component"
            .to_string(),
    )
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_LOAD,
        name: b"onnx_load\0",
        num_args: 1,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_INPUT_NAMES,
        name: b"onnx_input_names\0",
        num_args: 1,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_OUTPUT_NAMES,
        name: b"onnx_output_names\0",
        num_args: 1,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_RUN,
        name: b"onnx_run\0",
        num_args: 2,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_UNLOAD,
        name: b"onnx_unload\0",
        num_args: 1,
        deterministic: false,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
