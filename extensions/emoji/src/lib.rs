//! Emoji detection / extraction / lookup.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use unicode_segmentation::UnicodeSegmentation;

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

    const FID_COUNT: u64 = 1;
    const FID_EXTRACT: u64 = 2;
    const FID_STRIP: u64 = 3;
    const FID_FROM_SHORTCODE: u64 = 4;
    const FID_SHORTCODE: u64 = 5;
    const FID_NAME: u64 = 6;
    const FID_GROUP: u64 = 7;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn each_emoji_grapheme<F>(text: &str, mut f: F)
    where
        F: FnMut(&str),
    {
        for g in text.graphemes(true) {
            if emojis::get(g).is_some() {
                f(g);
            }
        }
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
                name: "emoji".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_COUNT, "emoji_count", 1),
                    s(FID_EXTRACT, "emoji_extract", 1),
                    s(FID_STRIP, "emoji_strip", 1),
                    s(FID_FROM_SHORTCODE, "emoji_from_shortcode", 1),
                    s(FID_SHORTCODE, "emoji_shortcode", 1),
                    s(FID_NAME, "emoji_name", 1),
                    s(FID_GROUP, "emoji_group", 1),
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
                FID_COUNT => {
                    let t = arg_text(&args, 0, "emoji_count")?;
                    let mut n = 0i64;
                    each_emoji_grapheme(&t, |_| n += 1);
                    Ok(SqlValue::Integer(n))
                }
                FID_EXTRACT => {
                    let t = arg_text(&args, 0, "emoji_extract")?;
                    let mut out: Vec<String> = Vec::new();
                    each_emoji_grapheme(&t, |g| out.push(g.to_string()));
                    Ok(SqlValue::Text(
                        serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string()),
                    ))
                }
                FID_STRIP => {
                    let t = arg_text(&args, 0, "emoji_strip")?;
                    let kept: String = t
                        .graphemes(true)
                        .filter(|g| emojis::get(g).is_none())
                        .collect();
                    Ok(SqlValue::Text(kept))
                }
                FID_FROM_SHORTCODE => {
                    let sc = arg_text(&args, 0, "emoji_from_shortcode")?;
                    // accept both ":sparkles:" and "sparkles"
                    let trimmed = sc.trim_matches(':');
                    match emojis::get_by_shortcode(trimmed) {
                        Some(e) => Ok(SqlValue::Text(e.as_str().to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_SHORTCODE => {
                    let t = arg_text(&args, 0, "emoji_shortcode")?;
                    match emojis::get(&t).and_then(|e| e.shortcode()) {
                        Some(sc) => Ok(SqlValue::Text(sc.to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_NAME => {
                    let t = arg_text(&args, 0, "emoji_name")?;
                    match emojis::get(&t) {
                        Some(e) => Ok(SqlValue::Text(e.name().to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_GROUP => {
                    let t = arg_text(&args, 0, "emoji_group")?;
                    match emojis::get(&t) {
                        Some(e) => Ok(SqlValue::Text(format!("{:?}", e.group()).to_lowercase())),
                        None => Ok(SqlValue::Null),
                    }
                }
                other => Err(format!("emoji: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
