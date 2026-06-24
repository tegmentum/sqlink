//! `.serialize` / `.deserialize`  Phase 5 follow-up: migrated
//! out of cli/src/dot.rs as a dot-command extension.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "dotcmd-aware",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::dot_command::{
        Guest as DotCommandGuest, InvokeContext, InvokeResult, StateDelta,
    };
    use bindings::exports::sqlite::extension::metadata::{
        DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::cli_stdout;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_SERIALIZE:   u64 = 1;
    const FID_DESERIALIZE: u64 = 2;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let spec = |id, name: &str, summary: &str, usage: &str, help: &str| DotCommandSpec {
                id,
                name: name.into(),
                version: env!("CARGO_PKG_VERSION").into(),
                summary: summary.into(),
                usage: usage.into(),
                help: help.into(),
                examples: alloc::vec![],
                requires_write: false,
                no_args: false,
            };
            Manifest {
                name: "serialize-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![
                    spec(FID_SERIALIZE, "serialize",
                         "Dump the main db to a file",
                         "serialize FILE [DB]",
                         "Reads bytes via the new spi.serialize-db host call \
                          (which uses sqlite3_serialize against the extension's \
                          spi connection, opened to the same db file as the cli) \
                          and writes them to FILE. DB defaults to main."),
                    spec(FID_DESERIALIZE, "deserialize",
                         "Load a file into the main db (replacing contents)",
                         "deserialize FILE [DB]",
                         "Reads FILE's bytes off disk and emits a \
                          conn/deserialize/<DB> state-delta the cli applies via \
                          sqlite3_deserialize on its main connection. DB defaults \
                          to main."),
                ],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("serialize-cli: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            Ok(match func_id {
                FID_SERIALIZE   => cmd_serialize(ctx.args.trim()),
                FID_DESERIALIZE => cmd_deserialize(ctx.args.trim()),
                _ => return Err(SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("serialize-cli: unknown func id {func_id}"),
                }),
            })
        }
    }

    fn parse_file_and_db(arg: &str) -> Option<(String, String)> {
        let mut parts = arg.split_whitespace();
        let file = parts.next()?;
        let db = parts.next().unwrap_or("main");
        Some((file.to_string(), db.to_string()))
    }

    fn cmd_serialize(arg: &str) -> InvokeResult {
        let Some((file, db)) = parse_file_and_db(arg) else {
            return err(".serialize FILE [DB]".into());
        };
        let bytes = match spi::serialize_db(&db) {
            Ok(b) => b,
            Err(e) => return err(format!(".serialize: {}", e.message)),
        };
        match std::fs::write(&file, &bytes) {
            Ok(()) => {
                cli_stdout::write(&format!("wrote {} bytes to {file}\n", bytes.len()));
                ok()
            }
            Err(e) => err(format!(".serialize: write {file:?}: {e}")),
        }
    }

    fn cmd_deserialize(arg: &str) -> InvokeResult {
        let Some((file, db)) = parse_file_and_db(arg) else {
            return err(".deserialize FILE [DB]".into());
        };
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) => return err(format!(".deserialize: read {file:?}: {e}")),
        };
        let len = bytes.len();
        // SqlValue::Blob round-trips through the state-delta wire
        // as a JSON-quoted X'<hex>' literal; the cli's
        // parse_blob_hex decodes it back to raw bytes.
        let result = InvokeResult {
            text: format!("read {} bytes from {file} into {db}\n", len),
            state_deltas: alloc::vec![StateDelta {
                key: format!("conn/deserialize/{db}"),
                value: SqlValue::Blob(bytes),
            }],
            ok: true,
            exit_code: 0,
        };
        result
    }

    fn ok() -> InvokeResult {
        InvokeResult {
            text: String::new(),
            state_deltas: alloc::vec![],
            ok: true,
            exit_code: 0,
        }
    }

    fn err(message: String) -> InvokeResult {
        InvokeResult {
            text: format!("{message}\n"),
            state_deltas: alloc::vec![],
            ok: false,
            exit_code: 1,
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
