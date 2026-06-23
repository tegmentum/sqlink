//! `.sha3sum`  Phase 5 follow-up: migrated out of
//! cli/src/dot.rs into a wasm dot-command extension.
//!
//! Walks the schema, computes Sha3_256 over each table's rows
//! in a canonical encoding (column name + NUL + typed value +
//! NUL, row-separator 0x01), prints one line per table.
//!
//! Not bit-identical to upstream sqlite3's `.sha3sum` (which
//! uses its own encoding); stable for our build's use as a
//! cross-table comparison or change-detection tool.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use sha3::{Digest, Sha3_256};

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "dotcmd-aware",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::dot_command::{
        Guest as DotCommandGuest, InvokeContext, InvokeResult,
    };
    use bindings::exports::sqlite::extension::metadata::{
        DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::cli_stdout;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_SHA3SUM: u64 = 1;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "sha3sum-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![DotCommandSpec {
                    id: FID_SHA3SUM,
                    name: "sha3sum".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    summary: "Sha3_256 of every table's contents".into(),
                    usage: "sha3sum [TABLE_OR_PATTERN]".into(),
                    help: "Without args (or with `*`), hashes every non-`sqlite_*` table. \
                           With NAME, hashes just that table. Output is one line per \
                           table: `<hex>  <name>`. The hash is over a canonical encoding \
                           of the rows (column-name + NUL + typed value + NUL, row \
                           separator 0x01); not bit-identical to upstream sqlite3's \
                           `.sha3sum`.".into(),
                    examples: alloc::vec![],
                    requires_write: false,
                    no_args: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("sha3sum-cli: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            if func_id != FID_SHA3SUM {
                return Err(SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("sha3sum-cli: unknown func id {func_id}"),
                });
            }
            let text = sha3sum_dispatch(ctx.args.trim());
            Ok(InvokeResult {
                text,
                state_deltas: alloc::vec![],
                ok: true,
                exit_code: 0,
            })
        }
    }

    fn sha3sum_dispatch(arg: &str) -> String {
        let where_clause = if arg.is_empty() || arg == "*" {
            "type='table' AND name NOT LIKE 'sqlite_%'".to_string()
        } else {
            let esc = arg.replace('\'', "''");
            format!("type='table' AND name='{esc}'")
        };
        let sql = format!(
            "SELECT name FROM sqlite_master WHERE {where_clause} ORDER BY name"
        );
        let tables = match spi::execute(&sql, &[]) {
            Ok(r) => r.rows,
            Err(e) => return format!(".sha3sum: list tables: {}\n", e.message),
        };
        if tables.is_empty() {
            return "(no tables matched)\n".into();
        }
        let mut out = String::new();
        for row in tables {
            let tbl = match row.into_iter().next() {
                Some(SqlValue::Text(s)) => s,
                _ => continue,
            };
            out.push_str(&hash_table(&tbl));
        }
        out
    }

    fn hash_table(tbl: &str) -> String {
        let escaped = tbl.replace('"', "\"\"");
        let select = format!("SELECT * FROM \"{escaped}\"");
        let result = match spi::execute(&select, &[]) {
            Ok(r) => r,
            Err(e) => return format!("Error reading {tbl}: {}\n", e.message),
        };
        let mut hasher = Sha3_256::new();
        for row in &result.rows {
            for (i, v) in row.iter().enumerate() {
                let col_name = result
                    .columns
                    .get(i)
                    .map(|c| c.as_bytes())
                    .unwrap_or(b"");
                hasher.update(col_name);
                hasher.update(b"\0");
                match v {
                    SqlValue::Null       => hasher.update(b"N"),
                    SqlValue::Integer(n) => { hasher.update(b"I"); hasher.update(&n.to_le_bytes()); }
                    SqlValue::Real(r)    => { hasher.update(b"R"); hasher.update(&r.to_le_bytes()); }
                    SqlValue::Text(t)    => { hasher.update(b"T"); hasher.update(t.as_bytes()); }
                    SqlValue::Blob(b)    => { hasher.update(b"B"); hasher.update(b); }
                }
                hasher.update(b"\0");
            }
            hasher.update(b"\x01");
        }
        let digest = hasher.finalize();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        format!("{hex}  {tbl}\n")
    }

    #[allow(dead_code)]
    fn _keep_imports_live() { cli_stdout::write(""); }

    bindings::export!(Ext with_types_in bindings);
}
