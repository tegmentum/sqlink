//! Embed path for sentiment. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};
use vader_sentiment::SentimentIntensityAnalyzer;

const FID_SCORE: u64 = 1;
const FID_LABEL: u64 = 2;
const FID_BREAKDOWN: u64 = 3;
const FID_POSITIVE: u64 = 4;
const FID_NEGATIVE: u64 = 5;
const FID_NEUTRAL: u64 = 6;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
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

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let t = arg_text(&args, 0, "sentiment")?;
    let (pos, neu, neg, compound) = score_all(&t);

    match func_id {
        FID_SCORE => Ok(SqlValueOwned::Real(compound)),
        FID_LABEL => Ok(SqlValueOwned::Text(label_from_compound(compound).to_string())),
        FID_BREAKDOWN => {
            let body = serde_json::json!({
                "pos": pos,
                "neu": neu,
                "neg": neg,
                "compound": compound,
            });
            Ok(SqlValueOwned::Text(body.to_string()))
        }
        FID_POSITIVE => Ok(SqlValueOwned::Real(pos)),
        FID_NEGATIVE => Ok(SqlValueOwned::Real(neg)),
        FID_NEUTRAL => Ok(SqlValueOwned::Real(neu)),
        other => Err(format!("sentiment: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_SCORE,     name: b"sentiment_score\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_LABEL,     name: b"sentiment_label\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_BREAKDOWN, name: b"sentiment_breakdown\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_POSITIVE,  name: b"sentiment_positive\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NEGATIVE,  name: b"sentiment_negative\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NEUTRAL,   name: b"sentiment_neutral\0",   num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
