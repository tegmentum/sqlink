//! SQL parsing scalars via `sqlparser`.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::ops::ControlFlow;

    use sqlparser::ast::{ObjectName, Statement, VisitorMut};
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_VALIDATE: u64 = 1;
    const FID_STMT_TYPE: u64 = 2;
    const FID_STMT_COUNT: u64 = 3;
    const FID_TABLES: u64 = 4;
    const FID_READONLY: u64 = 5;
    const FID_DIALECT: u64 = 6;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
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
        let mut seen = std::collections::HashSet::new();
        c.out.retain(|t| seen.insert(t.clone()));
        c.out
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "sqlparse".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "sql_validate", 1),
                    s(FID_STMT_TYPE, "sql_statement_type", 1),
                    s(FID_STMT_COUNT, "sql_statement_count", 1),
                    s(FID_TABLES, "sql_tables", 1),
                    s(FID_READONLY, "sql_is_readonly", 1),
                    s(FID_DIALECT, "sql_dialect", 0),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: Some("sqlparse".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.sqlparse".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_DIALECT => Ok(SqlValue::Text("generic".to_string())),
                FID_VALIDATE => {
                    let t = arg_text(&args, 0, "sql_validate")?;
                    Ok(SqlValue::Integer(parse(&t).is_ok() as i64))
                }
                FID_STMT_COUNT => {
                    let t = arg_text(&args, 0, "sql_statement_count")?;
                    match parse(&t) {
                        Ok(stmts) => Ok(SqlValue::Integer(stmts.len() as i64)),
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_STMT_TYPE => {
                    let t = arg_text(&args, 0, "sql_statement_type")?;
                    match parse(&t) {
                        Ok(stmts) if !stmts.is_empty() => {
                            Ok(SqlValue::Text(statement_type(&stmts[0]).to_string()))
                        }
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_TABLES => {
                    let t = arg_text(&args, 0, "sql_tables")?;
                    match parse(&t) {
                        Ok(mut stmts) => {
                            let names = collect_tables(&mut stmts);
                            Ok(SqlValue::Text(
                                serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string()),
                            ))
                        }
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_READONLY => {
                    let t = arg_text(&args, 0, "sql_is_readonly")?;
                    match parse(&t) {
                        Ok(stmts) if !stmts.is_empty() => {
                            Ok(SqlValue::Integer(stmts.iter().all(is_readonly) as i64))
                        }
                        _ => Ok(SqlValue::Null),
                    }
                }
                other => Err(format!("sqlparse: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
