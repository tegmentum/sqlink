//! Substrate probe for sqlink#445  spi.spawn-build capability.
//!
//! Exposes a single scalar function:
//!
//!   spawn_build_probe(crate_root TEXT) -> TEXT
//!       Calls build.spawn-build(crate_root, none, []). Returns
//!       the produced binary-path on success, 'ERR: <message>'
//!       on failure.
//!
//! The smoke test drives it against a generated tiny hello-world
//! crate written to a tempdir; the cli's --grant=spawn-build must
//! be set or the host's policy gate refuses the load.
//!
//! This is the minimal-shape consumer of the new SPI: a single
//! scalar that immediately returns whatever the host gave it. The
//! bundle-cli extension (#446) is the production consumer.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

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
    use bindings::sqlite::extension::build;
    use bindings::sqlite::extension::policy::Capability;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_PROBE: u64 = 1;

    struct Ext;

    bindings::export!(Ext with_types_in bindings);

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "spawn-build-probe".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![ScalarFunctionSpec {
                    id: FID_PROBE,
                    name: "spawn_build_probe".into(),
                    num_args: 1,
                    func_flags: FunctionFlags::DIRECT_ONLY,
                }],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                // spawn_build_probe calls build.spawn-build, which
                // is gated by capability::spawn-build. The host
                // fails the load if the operator's --grant list
                // is missing it.
                declared_capabilities: alloc::vec![Capability::SpawnBuild],
                optional_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_PROBE => {
                    let crate_root = match args.into_iter().next() {
                        Some(SqlValue::Text(s)) => s,
                        _ => {
                            return Err(
                                "spawn_build_probe: crate_root arg must be TEXT".into()
                            )
                        }
                    };
                    match build::spawn_build(&crate_root, None, &[], None, &[]) {
                        Ok(out) => Ok(SqlValue::Text(out.binary_path)),
                        Err(e) => Ok(SqlValue::Text(format!("ERR: {}", e.message))),
                    }
                }
                _ => Err(format!("spawn-build-probe: unknown func_id {func_id}")),
            }
        }
    }
}
