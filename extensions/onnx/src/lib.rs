//! ONNX inference scalars via tract-onnx.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicI64, Ordering};

    use tract_onnx::prelude::*;

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

    const FID_LOAD: u64 = 1;
    const FID_INPUT_NAMES: u64 = 2;
    const FID_OUTPUT_NAMES: u64 = 3;
    const FID_RUN: u64 = 4;
    const FID_UNLOAD: u64 = 5;

    struct Session {
        model: std::sync::Arc<TypedRunnableModel>,
        input_names: Vec<String>,
        output_names: Vec<String>,
        input_shape: Vec<usize>,
    }

    thread_local! {
        static SESSIONS: RefCell<HashMap<i64, Session>> = RefCell::new(HashMap::new());
        static NEXT_ID: AtomicI64 = AtomicI64::new(1);
    }

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    fn load_model(path: &str) -> Result<Session, String> {
        let model = tract_onnx::onnx()
            .model_for_path(path)
            .map_err(|e| format!("onnx_load: parse {path}: {e}"))?;
        let input_names: Vec<String> = (0..model.inputs.len())
            .filter_map(|i| {
                let id = model.inputs[i].node;
                model.node(id).name.clone().into()
            })
            .collect();
        let output_names: Vec<String> = (0..model.outputs.len())
            .filter_map(|i| {
                let id = model.outputs[i].node;
                model.node(id).name.clone().into()
            })
            .collect();
        let optimized = model.into_optimized().map_err(|e| {
            format!(
                "onnx_load: optimize: {e}  model may have unresolved shape facts; \
                 v1 requires fully-resolved shapes"
            )
        })?;
        let input_fact = optimized
            .input_fact(0)
            .map_err(|e| format!("onnx_load: input fact: {e}"))?
            .clone();
        let input_shape: Vec<usize> = input_fact
            .shape
            .as_concrete()
            .ok_or_else(|| {
                "onnx_load: unsupported: input shape is symbolic; v1 needs concrete shapes"
                    .to_string()
            })?
            .iter()
            .copied()
            .collect();
        let runnable = optimized
            .into_runnable()
            .map_err(|e| format!("onnx_load: into_runnable: {e}"))?;
        Ok(Session {
            model: runnable,
            input_names,
            output_names,
            input_shape,
        })
    }

    fn parse_f32_array(json: &str) -> Result<Vec<f32>, String> {
        let v: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("onnx_run: parse input JSON: {e}"))?;
        let arr = v
            .as_array()
            .ok_or_else(|| "onnx_run: input must be a JSON array of numbers".to_string())?;
        arr.iter()
            .map(|x| {
                x.as_f64()
                    .ok_or_else(|| "onnx_run: non-numeric value in input".to_string())
                    .map(|n| n as f32)
            })
            .collect()
    }

    fn run_session(s: &Session, input: Vec<f32>) -> Result<String, String> {
        let expected: usize = s.input_shape.iter().product();
        if expected != input.len() {
            return Err(format!(
                "onnx_run: input length {} does not match model input shape product {} ({:?})",
                input.len(),
                expected,
                s.input_shape
            ));
        }
        let tensor = tract_onnx::prelude::Tensor::from_shape(&s.input_shape, &input)
            .map_err(|e| format!("onnx_run: build tensor: {e}"))?;
        let result = s
            .model
            .run(tvec!(tensor.into()))
            .map_err(|e| format!("onnx_run: inference: {e}"))?;
        let first = result
            .into_iter()
            .next()
            .ok_or_else(|| "onnx_run: model produced no outputs".to_string())?;
        let view = first
            .to_plain_array_view::<f32>()
            .map_err(|e| format!("onnx_run: output is not f32: {e}"))?;
        let out: Vec<f64> = view.iter().map(|x| *x as f64).collect();
        Ok(serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string()))
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: FunctionFlags::empty(),
            };
            Manifest {
                name: "onnx".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_LOAD, "onnx_load", 1),
                    s(FID_INPUT_NAMES, "onnx_input_names", 1),
                    s(FID_OUTPUT_NAMES, "onnx_output_names", 1),
                    s(FID_RUN, "onnx_run", 2),
                    s(FID_UNLOAD, "onnx_unload", 1),
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
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_LOAD => {
                    let path = arg_text(&args, 0, "onnx_load")?;
                    let session = load_model(&path)?;
                    let id = NEXT_ID.with(|n| n.fetch_add(1, Ordering::Relaxed));
                    SESSIONS.with(|m| m.borrow_mut().insert(id, session));
                    Ok(SqlValue::Integer(id))
                }
                FID_INPUT_NAMES => {
                    let id = arg_int(&args, 0, "onnx_input_names")?;
                    SESSIONS.with(|m| {
                        let map = m.borrow();
                        let s = map
                            .get(&id)
                            .ok_or_else(|| format!("onnx_input_names: no handle {id}"))?;
                        Ok(SqlValue::Text(
                            serde_json::to_string(&s.input_names)
                                .unwrap_or_else(|_| "[]".to_string()),
                        ))
                    })
                }
                FID_OUTPUT_NAMES => {
                    let id = arg_int(&args, 0, "onnx_output_names")?;
                    SESSIONS.with(|m| {
                        let map = m.borrow();
                        let s = map
                            .get(&id)
                            .ok_or_else(|| format!("onnx_output_names: no handle {id}"))?;
                        Ok(SqlValue::Text(
                            serde_json::to_string(&s.output_names)
                                .unwrap_or_else(|_| "[]".to_string()),
                        ))
                    })
                }
                FID_RUN => {
                    let id = arg_int(&args, 0, "onnx_run")?;
                    let input_json = arg_text(&args, 1, "onnx_run")?;
                    let input = parse_f32_array(&input_json)?;
                    let json = SESSIONS.with(|m| {
                        let map = m.borrow();
                        let s = map
                            .get(&id)
                            .ok_or_else(|| format!("onnx_run: no handle {id}"))?;
                        run_session(s, input)
                    })?;
                    Ok(SqlValue::Text(json))
                }
                FID_UNLOAD => {
                    let id = arg_int(&args, 0, "onnx_unload")?;
                    let removed = SESSIONS.with(|m| m.borrow_mut().remove(&id).is_some());
                    Ok(SqlValue::Integer(removed as i64))
                }
                other => Err(format!("onnx: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
