//! Embed path for graphql. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table. Mirrors the
//! WIT path's call logic so the two can't drift.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use graphql_parser::query::{
    parse_query, Definition, Document, OperationDefinition, Selection, SelectionSet,
};
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_OPERATIONS: u64 = 2;
const FID_FIELDS: u64 = 3;
const FID_NORMALIZE: u64 = 4;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn parse(text: &str) -> Result<Document<'_, &'_ str>, String> {
    parse_query::<&str>(text).map_err(|e| format!("graphql parse: {e}"))
}

fn operations_json<'a>(doc: &Document<'a, &'a str>) -> String {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for def in &doc.definitions {
        if let Definition::Operation(op) = def {
            let (name, kind) = match op {
                OperationDefinition::Query(q) => (q.name.unwrap_or("").to_string(), "query"),
                OperationDefinition::Mutation(m) => (m.name.unwrap_or("").to_string(), "mutation"),
                OperationDefinition::Subscription(s) => {
                    (s.name.unwrap_or("").to_string(), "subscription")
                }
                OperationDefinition::SelectionSet(_) => (String::new(), "query"),
            };
            out.push(serde_json::json!({"name": name, "type": kind}));
        }
    }
    serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
}

fn collect_fields_inner<'a>(ss: &SelectionSet<'a, &'a str>, prefix: &str, out: &mut Vec<String>) {
    for item in &ss.items {
        if let Selection::Field(f) = item {
            let path = if prefix.is_empty() {
                f.name.to_string()
            } else {
                format!("{prefix}.{}", f.name)
            };
            out.push(path.clone());
            if !f.selection_set.items.is_empty() {
                collect_fields_inner(&f.selection_set, &path, out);
            }
        }
    }
}

fn fields_json<'a>(doc: &Document<'a, &'a str>) -> String {
    let mut paths: Vec<String> = Vec::new();
    for def in &doc.definitions {
        if let Definition::Operation(op) = def {
            let ss = match op {
                OperationDefinition::Query(q) => &q.selection_set,
                OperationDefinition::Mutation(m) => &m.selection_set,
                OperationDefinition::Subscription(s) => &s.selection_set,
                OperationDefinition::SelectionSet(s) => s,
            };
            collect_fields_inner(ss, "", &mut paths);
        }
    }
    serde_json::to_string(&paths).unwrap_or_else(|_| "[]".to_string())
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let q = arg_text(&args, 0, "gql")?;
    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(parse(&q).is_ok() as i64)),
        FID_OPERATIONS => match parse(&q) {
            Ok(doc) => Ok(SqlValueOwned::Text(operations_json(&doc))),
            Err(_) => Ok(SqlValueOwned::Null),
        },
        FID_FIELDS => match parse(&q) {
            Ok(doc) => Ok(SqlValueOwned::Text(fields_json(&doc))),
            Err(_) => Ok(SqlValueOwned::Null),
        },
        FID_NORMALIZE => match parse(&q) {
            Ok(doc) => Ok(SqlValueOwned::Text(format!("{doc}"))),
            Err(_) => Ok(SqlValueOwned::Null),
        },
        other => Err(format!("graphql: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"gql_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_OPERATIONS,
        name: b"gql_operations\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FIELDS,
        name: b"gql_fields\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NORMALIZE,
        name: b"gql_normalize\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
