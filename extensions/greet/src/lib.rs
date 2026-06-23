//! Hello-world dot-command extension. The simplest possible
//! `dotcmd-aware` consumer  registers a single `.greet NAME`
//! command, writes "hello, NAME!\n" to the cli's stdout stream,
//! returns success. Exists as Phase-1 proof that the WIT
//! contract + host wiring are sound; not a useful feature.

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
        Guest as DotCommandGuest, InvokeContext, InvokeResult,
    };
    use bindings::exports::sqlite::extension::metadata::{
        DotCommandExample, DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::cli_stdout;
    use bindings::sqlite::extension::types::{SqliteError, SqlValue};

    const FID_GREET: u64 = 1;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "greet".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![DotCommandSpec {
                    id: FID_GREET,
                    name: "greet".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    summary: "Greet someone by name".to_string(),
                    usage: "greet <name>".to_string(),
                    help: "Writes 'hello, <name>!' to stdout.\n\
                           Defaults to 'world' when no name is given."
                        .to_string(),
                    examples: alloc::vec![
                        DotCommandExample {
                            description: "Default".to_string(),
                            command: ".greet".to_string(),
                        },
                        DotCommandExample {
                            description: "With a name".to_string(),
                            command: ".greet alice".to_string(),
                        },
                    ],
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
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("greet: no scalar functions exported".to_string())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(
            func_id: u64,
            ctx: InvokeContext,
        ) -> Result<InvokeResult, SqliteError> {
            if func_id != FID_GREET {
                return Err(SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("greet: unknown func id {func_id}"),
                });
            }
            let name = ctx.args.trim();
            let target = if name.is_empty() { "world" } else { name };
            cli_stdout::write(&format!("hello, {target}!\n"));
            Ok(InvokeResult {
                text: String::new(),
                state_deltas: alloc::vec![],
                ok: true,
                exit_code: 0,
            })
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
