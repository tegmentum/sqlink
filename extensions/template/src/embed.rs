//! Embed path for template. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_RENDER: u64 = 1;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_RENDER => {
            let tmpl = arg_text(&args, 0, "template_render")?;
            let ctx_json = arg_text(&args, 1, "template_render")?;
            let ctx: serde_json::Value = serde_json::from_str(&ctx_json)
                .map_err(|e| format!("template_render: parse context JSON: {e}"))?;
            let env = minijinja::Environment::new();
            let rendered = env
                .render_str(&tmpl, ctx)
                .map_err(|e| format!("template_render: {e}"))?;
            Ok(SqlValueOwned::Text(rendered))
        }
        other => Err(format!("template: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[ScalarSpec {
    func_id: FID_RENDER,
    name: b"template_render\0",
    num_args: 2,
    deterministic: true,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
