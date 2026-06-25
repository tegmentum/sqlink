//! Embed path for case. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use heck::{
    ToKebabCase, ToLowerCamelCase, ToPascalCase, ToShoutyKebabCase, ToShoutySnakeCase, ToSnakeCase,
    ToTitleCase, ToTrainCase,
};
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_SNAKE: u64 = 1;
const FID_KEBAB: u64 = 2;
const FID_CAMEL: u64 = 3;
const FID_PASCAL: u64 = 4;
const FID_SCR_SNAKE: u64 = 5;
const FID_SCR_KEBAB: u64 = 6;
const FID_TITLE: u64 = 7;
const FID_TRAIN: u64 = 8;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let t = arg_text(&args, 0, "case")?;
    let out = match func_id {
        FID_SNAKE => t.to_snake_case(),
        FID_KEBAB => t.to_kebab_case(),
        FID_CAMEL => t.to_lower_camel_case(),
        FID_PASCAL => t.to_pascal_case(),
        FID_SCR_SNAKE => t.to_shouty_snake_case(),
        FID_SCR_KEBAB => t.to_shouty_kebab_case(),
        FID_TITLE => t.to_title_case(),
        FID_TRAIN => t.to_train_case(),
        other => return Err(format!("case: unknown func id {other}")),
    };
    Ok(SqlValueOwned::Text(out))
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_SNAKE,
        name: b"to_snake_case\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_KEBAB,
        name: b"to_kebab_case\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CAMEL,
        name: b"to_camel_case\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PASCAL,
        name: b"to_pascal_case\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SCR_SNAKE,
        name: b"to_screaming_snake\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SCR_KEBAB,
        name: b"to_screaming_kebab\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TITLE,
        name: b"to_title_case\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TRAIN,
        name: b"to_train_case\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
