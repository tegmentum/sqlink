//! Embed path for sqlparse. All FFI glue is in `sqlite-embed`; this
//! is just the per-extension dispatch + ScalarSpec table.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use core::ops::ControlFlow;

use sqlparser::ast::{ObjectName, Statement, VisitorMut};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_STMT_TYPE: u64 = 2;
const FID_STMT_COUNT: u64 = 3;
const FID_TABLES: u64 = 4;
const FID_READONLY: u64 = 5;
const FID_DIALECT: u64 = 6;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn parse(text: &str) -> Result<Vec<Statement>, sqlparser::parser::ParserError> {
    Parser::parse_sql(&GenericDialect, text)
}

fn statement_type(s: &Statement) -> &'static str {
    match s {
        Statement::Query(_) => "SELECT",
        Statement::Insert { .. } => "INSERT",
        Statement::Update { .. } => "UPDATE",
        Statement::Delete { .. } => "DELETE",
        Statement::CreateTable { .. } => "CREATE TABLE",
        Statement::CreateView { .. } => "CREATE VIEW",
        Statement::CreateIndex { .. } => "CREATE INDEX",
        Statement::Drop { .. } => "DROP",
        Statement::AlterTable { .. } => "ALTER TABLE",
        Statement::Truncate { .. } => "TRUNCATE",
        Statement::Explain { .. } => "EXPLAIN",
        Statement::Commit { .. } => "COMMIT",
        Statement::Rollback { .. } => "ROLLBACK",
        Statement::StartTransaction { .. } => "BEGIN",
        Statement::Savepoint { .. } => "SAVEPOINT",
        _ => "OTHER",
    }
}

fn is_readonly(s: &Statement) -> bool {
    matches!(
        s,
        Statement::Query(_) | Statement::Explain { .. } | Statement::ExplainTable { .. }
    )
}

struct TableCollector {
    out: Vec<String>,
}

impl VisitorMut for TableCollector {
    type Break = ();
    fn pre_visit_relation(&mut self, name: &mut ObjectName) -> ControlFlow<()> {
        self.out.push(name.to_string());
        ControlFlow::Continue(())
    }
}

fn collect_tables(stmts: &mut Vec<Statement>) -> Vec<String> {
    use sqlparser::ast::VisitMut;
    let mut c = TableCollector { out: Vec::new() };
    for s in stmts.iter_mut() {
        let _ = s.visit(&mut c);
    }
    // de-dup while preserving order
    let mut seen = BTreeSet::new();
    c.out.retain(|t| seen.insert(t.clone()));
    c.out
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_DIALECT => Ok(SqlValueOwned::Text("generic".to_string())),
        FID_VALIDATE => {
            let t = arg_text(&args, 0, "sql_validate")?;
            Ok(SqlValueOwned::Integer(parse(&t).is_ok() as i64))
        }
        FID_STMT_COUNT => {
            let t = arg_text(&args, 0, "sql_statement_count")?;
            match parse(&t) {
                Ok(stmts) => Ok(SqlValueOwned::Integer(stmts.len() as i64)),
                Err(_) => Ok(SqlValueOwned::Null),
            }
        }
        FID_STMT_TYPE => {
            let t = arg_text(&args, 0, "sql_statement_type")?;
            match parse(&t) {
                Ok(stmts) if !stmts.is_empty() => {
                    Ok(SqlValueOwned::Text(statement_type(&stmts[0]).to_string()))
                }
                _ => Ok(SqlValueOwned::Null),
            }
        }
        FID_TABLES => {
            let t = arg_text(&args, 0, "sql_tables")?;
            match parse(&t) {
                Ok(mut stmts) => {
                    let names = collect_tables(&mut stmts);
                    Ok(SqlValueOwned::Text(
                        serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string()),
                    ))
                }
                Err(_) => Ok(SqlValueOwned::Null),
            }
        }
        FID_READONLY => {
            let t = arg_text(&args, 0, "sql_is_readonly")?;
            match parse(&t) {
                Ok(stmts) if !stmts.is_empty() => Ok(SqlValueOwned::Integer(
                    stmts.iter().all(is_readonly) as i64,
                )),
                _ => Ok(SqlValueOwned::Null),
            }
        }
        other => Err(format!("sqlparse: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_VALIDATE,   name: b"sql_validate\0",         num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_STMT_TYPE,  name: b"sql_statement_type\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_STMT_COUNT, name: b"sql_statement_count\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TABLES,     name: b"sql_tables\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_READONLY,   name: b"sql_is_readonly\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_DIALECT,    name: b"sql_dialect\0",          num_args: 0, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
