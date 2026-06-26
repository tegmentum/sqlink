//! BPE tokenizer wrapping tiktoken-rs's cl100k_base encoding.
//! The vocab is embedded in the crate so the .wasm component
//! is self-contained (~3 MB tokenizer payload).

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::OnceCell;
use std::cell::RefCell;

thread_local! {
    static ENCODER: RefCell<OnceCell<tiktoken_rs::CoreBPE>> = RefCell::new(OnceCell::new());
}

fn with_encoder<R>(f: impl FnOnce(&tiktoken_rs::CoreBPE) -> R) -> Result<R, String> {
    ENCODER.with(|cell| {
        let cell = cell.borrow();
        if cell.get().is_none() {
            let enc = tiktoken_rs::cl100k_base()
                .map_err(|e| alloc::format!("bpe: load cl100k_base: {e}"))?;
            let _ = cell.set(enc);
        }
        Ok(f(cell.get().unwrap()))
    })
}

pub fn encode(text: &str) -> Result<Vec<u32>, String> {
    with_encoder(|enc| enc.encode_with_special_tokens(text))
}

pub fn decode(ids: &[u32]) -> Result<String, String> {
    let result = with_encoder(|enc| enc.decode(ids.to_vec()))?;
    result.map_err(|e| alloc::format!("bpe_decode: {e}"))
}

pub fn count_tokens(text: &str) -> Result<usize, String> {
    Ok(encode(text)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_known_short_text() {
        // tiktoken cl100k_base("hello world") yields the
        // canonical 2-token sequence [15339, 1917].
        let ids = encode("hello world").unwrap();
        assert_eq!(ids, alloc::vec![15339, 1917]);
    }

    #[test]
    fn round_trip() {
        let text = "The quick brown fox jumps over the lazy dog.";
        let ids = encode(text).unwrap();
        let back = decode(&ids).unwrap();
        assert_eq!(back, text);
    }

    #[test]
    fn count_matches_encode_len() {
        let t = "tokens have a count";
        let ids = encode(t).unwrap();
        assert_eq!(count_tokens(t).unwrap(), ids.len());
    }
}

// wasm_export is gated off in embed builds  the WIT export
// symbols would collide with any other embedded extension's.
// See PLAN-embed-extensions.md.
#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
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
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_ENCODE: u64 = 1;
    const FID_DECODE: u64 = 2;
    const FID_COUNT: u64 = 3;
    const FID_MODEL: u64 = 4;

    struct Ext;

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
                name: "bpe".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE, "bpe_encode", 1),
                    s(FID_DECODE, "bpe_decode", 1),
                    s(FID_COUNT, "bpe_count_tokens", 1),
                    s(FID_MODEL, "bpe_model_name", 0),
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
                preferred_prefix: Some("bpe".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.bpe".into()),
                typed_values: Vec::new(),
            }
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_MODEL => Ok(SqlValue::Text("cl100k_base".to_string())),
                FID_ENCODE => {
                    let t = arg_text(&args, 0, "bpe_encode")?;
                    let ids = super::encode(&t)?;
                    let json: Vec<serde_json::Value> = ids
                        .into_iter()
                        .map(|n| serde_json::Value::Number((n as u64).into()))
                        .collect();
                    Ok(SqlValue::Text(serde_json::Value::Array(json).to_string()))
                }
                FID_DECODE => {
                    let s = arg_text(&args, 0, "bpe_decode")?;
                    let v: serde_json::Value = serde_json::from_str(&s)
                        .map_err(|e| format!("bpe_decode: parse JSON: {e}"))?;
                    let arr = v
                        .as_array()
                        .ok_or_else(|| "bpe_decode: expected JSON array".to_string())?;
                    let ids: Vec<u32> = arr
                        .iter()
                        .filter_map(|n| n.as_u64().map(|n| n as u32))
                        .collect();
                    super::decode(&ids).map(SqlValue::Text)
                }
                FID_COUNT => {
                    let t = arg_text(&args, 0, "bpe_count_tokens")?;
                    super::count_tokens(&t).map(|n| SqlValue::Integer(n as i64))
                }
                other => Err(format!("bpe: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
