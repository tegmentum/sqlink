//! VADER-based sentiment scoring.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use vader_sentiment::SentimentIntensityAnalyzer;

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

    const FID_SCORE: u64 = 1;
    const FID_LABEL: u64 = 2;
    const FID_BREAKDOWN: u64 = 3;
    const FID_POSITIVE: u64 = 4;
    const FID_NEGATIVE: u64 = 5;
    const FID_NEUTRAL: u64 = 6;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn score_all(text: &str) -> (f64, f64, f64, f64) {
        let analyzer = SentimentIntensityAnalyzer::new();
        let scores = analyzer.polarity_scores(text);
        let pos = *scores.get("pos").unwrap_or(&0.0);
        let neu = *scores.get("neu").unwrap_or(&0.0);
        let neg = *scores.get("neg").unwrap_or(&0.0);
        let compound = *scores.get("compound").unwrap_or(&0.0);
        (pos, neu, neg, compound)
    }

    fn label_from_compound(c: f64) -> &'static str {
        if c >= 0.05 {
            "positive"
        } else if c <= -0.05 {
            "negative"
        } else {
            "neutral"
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
                name: "sentiment".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_SCORE, "sentiment_score", 1),
                    s(FID_LABEL, "sentiment_label", 1),
                    s(FID_BREAKDOWN, "sentiment_breakdown", 1),
                    s(FID_POSITIVE, "sentiment_positive", 1),
                    s(FID_NEGATIVE, "sentiment_negative", 1),
                    s(FID_NEUTRAL, "sentiment_neutral", 1),
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
            let t = arg_text(&args, 0, "sentiment")?;
            let (pos, neu, neg, compound) = score_all(&t);

            match func_id {
                FID_SCORE => Ok(SqlValue::Real(compound)),
                FID_LABEL => Ok(SqlValue::Text(label_from_compound(compound).to_string())),
                FID_BREAKDOWN => {
                    let body = serde_json::json!({
                        "pos": pos,
                        "neu": neu,
                        "neg": neg,
                        "compound": compound,
                    });
                    Ok(SqlValue::Text(body.to_string()))
                }
                FID_POSITIVE => Ok(SqlValue::Real(pos)),
                FID_NEGATIVE => Ok(SqlValue::Real(neg)),
                FID_NEUTRAL => Ok(SqlValue::Real(neu)),
                other => Err(format!("sentiment: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
