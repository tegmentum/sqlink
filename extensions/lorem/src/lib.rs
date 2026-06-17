//! Lorem ipsum / filler text generation.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

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

    const FID_WORDS: u64 = 1;
    const FID_SENTENCES: u64 = 2;
    const FID_TITLE: u64 = 3;
    const FID_WORDS_SEEDED: u64 = 4;
    const FID_SENTENCES_SEEDED: u64 = 5;

    struct Ext;

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    fn clamp_count(n: i64) -> usize {
        if n < 0 {
            0
        } else if n > 100_000 {
            100_000
        } else {
            n as usize
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // NOT marked deterministic for the unseeded variants
            // they reseed each call. Seeded variants ARE deterministic
            // but use the same FID; SQLite caching of det fns would
            // produce surprising "same result every call" behavior
            // across rows. Safer to call out non-det at the SQL layer.
            let flags = FunctionFlags::empty();
            let det = FunctionFlags::DETERMINISTIC;
            Manifest {
                name: "lorem".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    ScalarFunctionSpec {
                        id: FID_WORDS,
                        name: "lorem_words".into(),
                        num_args: 1,
                        func_flags: flags,
                    },
                    ScalarFunctionSpec {
                        id: FID_SENTENCES,
                        name: "lorem_sentences".into(),
                        num_args: 1,
                        func_flags: flags,
                    },
                    ScalarFunctionSpec {
                        id: FID_TITLE,
                        name: "lorem_title".into(),
                        num_args: 0,
                        func_flags: flags,
                    },
                    ScalarFunctionSpec {
                        id: FID_WORDS_SEEDED,
                        name: "lorem_words_seeded".into(),
                        num_args: 2,
                        func_flags: det,
                    },
                    ScalarFunctionSpec {
                        id: FID_SENTENCES_SEEDED,
                        name: "lorem_sentences_seeded".into(),
                        num_args: 2,
                        func_flags: det,
                    },
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_WORDS => {
                    let n = clamp_count(arg_int(&args, 0, "lorem_words")?);
                    Ok(SqlValue::Text(lipsum::lipsum_words(n)))
                }
                FID_SENTENCES => {
                    let n = clamp_count(arg_int(&args, 0, "lorem_sentences")?);
                    Ok(SqlValue::Text(lipsum::lipsum(n)))
                }
                FID_TITLE => Ok(SqlValue::Text(lipsum::lipsum_title())),
                FID_WORDS_SEEDED => {
                    let n = clamp_count(arg_int(&args, 0, "lorem_words_seeded")?);
                    let seed = arg_int(&args, 1, "lorem_words_seeded")? as u64;
                    let rng = ChaCha20Rng::seed_from_u64(seed);
                    Ok(SqlValue::Text(lipsum::lipsum_words_with_rng(rng, n)))
                }
                FID_SENTENCES_SEEDED => {
                    let n = clamp_count(arg_int(&args, 0, "lorem_sentences_seeded")?);
                    let seed = arg_int(&args, 1, "lorem_sentences_seeded")? as u64;
                    let rng = ChaCha20Rng::seed_from_u64(seed);
                    Ok(SqlValue::Text(lipsum::lipsum_with_rng(rng, n)))
                }
                other => Err(format!("lorem: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
