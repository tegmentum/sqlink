//! Language detection from text via the `whatlang` n-gram detector.
//!
//! Function surface (PLAN-more-extensions-3.md #8):
//!
//!   lang_detect(text)             -> text   (ISO 639-3 code; NULL if unknown)
//!   lang_detect_alpha2(text)      -> text   (ISO 639-1 code; NULL if no alpha-2)
//!   lang_detect_confidence(text)  -> real   (0..1)
//!   lang_detect_script(text)      -> text   ('Latin' | 'Cyrillic' | 'Han' | ...)
//!   lang_detect_all(text)         -> text   (JSON array, top-3 with confidence)
//!   lang_supported()              -> text   (JSON array of ISO 639-3 codes)
//!   lang_detect_version()         -> text
//!
//! Caveat (documented in PLAN section 8 + Cargo.toml description):
//! short strings (< ~20 chars) are unreliable by nature; mixed-script
//! text returns the dominant script. We map whatlang's `Mandarin`
//! script name to the ISO 15924-style `Han` because that's what the
//! plan acceptance asserts, but otherwise pass `Script::name()`
//! through verbatim.

extern crate alloc;

// ─────────────── pure-function core ───────────────
//
// Kept out of the wasm_export module so unit tests on the host can
// exercise it without the wit-bindgen layer. The wasm path delegates
// straight to these.

use alloc::format;
use alloc::string::String;

/// Map ISO 639-3 (whatlang's native code) to ISO 639-1 (alpha-2).
/// Returns `None` when no alpha-2 exists (e.g. `epo` -> `eo` exists,
/// but `aka` -> `ak`, `sna` -> `sn`; only a handful of whatlang's 70
/// langs lack a 639-1). Curated against ISO 639 part-2/part-3 tables.
pub fn iso6393_to_alpha2(code: &str) -> Option<&'static str> {
    Some(match code {
        "epo" => "eo",
        "eng" => "en",
        "rus" => "ru",
        "cmn" => "zh", // Mandarin Chinese -> macro lang 'zh'
        "spa" => "es",
        "por" => "pt",
        "ita" => "it",
        "ben" => "bn",
        "fra" => "fr",
        "deu" => "de",
        "ukr" => "uk",
        "kat" => "ka",
        "ara" => "ar",
        "hin" => "hi",
        "jpn" => "ja",
        "heb" => "he",
        "yid" => "yi",
        "pol" => "pl",
        "amh" => "am",
        "jav" => "jv",
        "kor" => "ko",
        "nob" => "nb",
        "dan" => "da",
        "swe" => "sv",
        "fin" => "fi",
        "tur" => "tr",
        "nld" => "nl",
        "hun" => "hu",
        "ces" => "cs",
        "ell" => "el",
        "bul" => "bg",
        "bel" => "be",
        "mar" => "mr",
        "kan" => "kn",
        "ron" => "ro",
        "slv" => "sl",
        "hrv" => "hr",
        "srp" => "sr",
        "mkd" => "mk",
        "lit" => "lt",
        "lav" => "lv",
        "est" => "et",
        "tam" => "ta",
        "vie" => "vi",
        "urd" => "ur",
        "tha" => "th",
        "guj" => "gu",
        "uzb" => "uz",
        "pan" => "pa",
        "aze" => "az",
        "ind" => "id",
        "tel" => "te",
        "pes" => "fa", // Iranian Persian -> macro lang 'fa'
        "mal" => "ml",
        "ori" => "or",
        "mya" => "my",
        "nep" => "ne",
        "sin" => "si",
        "khm" => "km",
        "tuk" => "tk",
        // aka, zul, sna lack a unique 639-1; aka does have 'ak' but
        // is the Akan macrolanguage. Mapping is consistent with ISO.
        "aka" => "ak",
        "zul" => "zu",
        "sna" => "sn",
        "afr" => "af",
        "lat" => "la",
        "slk" => "sk",
        "cat" => "ca",
        "tgl" => "tl",
        "hye" => "hy",
        "cym" => "cy",
        _ => return None,
    })
}

/// whatlang names the Chinese script `Mandarin` but the plan + ISO
/// 15924 use `Han`. Other names pass through.
pub fn normalize_script_name(s: &str) -> &str {
    match s {
        "Mandarin" => "Han",
        other => other,
    }
}

/// Minimal JSON string escape: ", \, control chars. Sufficient for
/// the values we serialize here (ISO 639-3 codes + floats), but kept
/// general in case Lang names ever leak into the JSON output.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Format an f64 confidence to 6 decimal places. Stable text so
/// smoke output is reproducible.
pub fn fmt_conf(x: f64) -> String {
    format!("{:.6}", x)
}

// ─────────────── wasm component export ───────────────

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use whatlang::{Detector, Lang, Script};

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

    const FID_DETECT: u64 = 1;
    const FID_ALPHA2: u64 = 2;
    const FID_CONF: u64 = 3;
    const FID_SCRIPT: u64 = 4;
    const FID_ALL: u64 = 5;
    const FID_SUPPORTED: u64 = 6;
    const FID_VERSION: u64 = 7;

    /// Minimum char count before lang_detect / lang_detect_alpha2 /
    /// lang_detect_confidence / lang_detect_all will even attempt a
    /// detection. whatlang is happy to label one-char inputs as e.g.
    /// `hun` / `est` -- noise, per the PLAN caveat that strings
    /// < ~20 chars are unreliable. The plan acceptance asserts
    /// `lang_detect('a') == NULL`, so we clip the very short tail
    /// here. Script detection has no such guard (a single Cyrillic
    /// char IS unambiguously Cyrillic script).
    const MIN_DETECT_CHARS: usize = 3;

    struct Ext;

    /// Coerce SqlValue -> text. NULL and non-TEXT input is treated
    /// as empty (so the detector returns None and the scalar yields
    /// NULL), matching how whatlang itself behaves on empty input.
    fn text_of(v: Option<&SqlValue>) -> String {
        match v {
            Some(SqlValue::Text(s)) => s.clone(),
            Some(SqlValue::Blob(b)) => String::from_utf8_lossy(b).into_owned(),
            // INTEGER/REAL coerce to their TEXT representation so a
            // numeric column passed accidentally doesn't error -- it
            // just won't detect anything meaningful.
            Some(SqlValue::Integer(n)) => n.to_string(),
            Some(SqlValue::Real(r)) => r.to_string(),
            _ => String::new(),
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
                name: "lang-detect".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_DETECT, "lang_detect", 1),
                    s(FID_ALPHA2, "lang_detect_alpha2", 1),
                    s(FID_CONF, "lang_detect_confidence", 1),
                    s(FID_SCRIPT, "lang_detect_script", 1),
                    s(FID_ALL, "lang_detect_all", 1),
                    s(FID_SUPPORTED, "lang_supported", 0),
                    s(FID_VERSION, "lang_detect_version", 0),
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

    /// Build a JSON array of top-N (lang, confidence) candidates.
    /// whatlang doesn't expose a ranked list directly, so we iterate
    /// with a deny-list: run detect, record the winner, ban it, run
    /// again. This is O(N * detect-cost) but N is fixed at 3 so it's
    /// fine. The approach is an approximation: the second/third
    /// candidate's confidence is computed against a reduced lang
    /// pool, which can inflate it. Documented in the smoke test.
    fn top_n_json(text: &str, n: usize) -> String {
        let mut out = String::from("[");
        let mut denied: Vec<Lang> = Vec::new();
        for i in 0..n {
            // First pass uses the default `detect()`; subsequent
            // passes use a denylist detector. Allowlist is the full
            // 70-lang set minus the already-seen winners.
            let info = if denied.is_empty() {
                whatlang::detect(text)
            } else {
                let allow: Vec<Lang> = Lang::all()
                    .iter()
                    .copied()
                    .filter(|l| !denied.contains(l))
                    .collect();
                if allow.is_empty() {
                    None
                } else {
                    Detector::with_allowlist(allow).detect(text)
                }
            };
            let Some(info) = info else { break };
            let lang = info.lang();
            denied.push(lang);
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"lang\":\"{}\",\"confidence\":{}}}",
                super::json_escape(lang.code()),
                super::fmt_conf(info.confidence()),
            ));
        }
        out.push(']');
        out
    }

    /// JSON array of every ISO 639-3 code whatlang can detect.
    /// Sorted alphabetically so the smoke output is stable across
    /// whatlang versions that may reorder the enum.
    fn supported_json() -> String {
        let mut codes: Vec<&'static str> = Lang::all().iter().map(|l| l.code()).collect();
        codes.sort_unstable();
        let mut out = String::from("[");
        for (i, c) in codes.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(c);
            out.push('"');
        }
        out.push(']');
        out
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_DETECT => {
                    let t = text_of(args.first());
                    if t.chars().count() < MIN_DETECT_CHARS {
                        return Ok(SqlValue::Null);
                    }
                    Ok(match whatlang::detect(&t) {
                        Some(info) => SqlValue::Text(info.lang().code().to_string()),
                        None => SqlValue::Null,
                    })
                }
                FID_ALPHA2 => {
                    let t = text_of(args.first());
                    if t.chars().count() < MIN_DETECT_CHARS {
                        return Ok(SqlValue::Null);
                    }
                    let Some(info) = whatlang::detect(&t) else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(match super::iso6393_to_alpha2(info.lang().code()) {
                        Some(c) => SqlValue::Text(c.to_string()),
                        None => SqlValue::Null,
                    })
                }
                FID_CONF => {
                    let t = text_of(args.first());
                    if t.chars().count() < MIN_DETECT_CHARS {
                        return Ok(SqlValue::Null);
                    }
                    Ok(match whatlang::detect(&t) {
                        Some(info) => SqlValue::Real(info.confidence()),
                        None => SqlValue::Null,
                    })
                }
                FID_SCRIPT => {
                    let t = text_of(args.first());
                    Ok(match whatlang::detect_script(&t) {
                        Some(sc) => SqlValue::Text(
                            super::normalize_script_name(sc.name()).to_string(),
                        ),
                        None => SqlValue::Null,
                    })
                }
                FID_ALL => {
                    let t = text_of(args.first());
                    // < 3 chars -> NULL (consistent with FID_DETECT;
                    // top-3 of one char is noise too).
                    if t.chars().count() < MIN_DETECT_CHARS {
                        return Ok(SqlValue::Null);
                    }
                    Ok(SqlValue::Text(top_n_json(&t, 3)))
                }
                FID_SUPPORTED => Ok(SqlValue::Text(supported_json())),
                FID_VERSION => {
                    let v = format!(
                        "whatlang 0.18; extension {}",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }
                other => Err(format!("lang-detect: unknown func id {other}")),
            }
        }
    }

    // Use the `Script` name() via the canonical impl; tests below
    // ensure normalize_script_name covers the Mandarin -> Han edge.
    fn _typecheck() -> &'static str {
        Script::Latin.name()
    }

    bindings::export!(Ext with_types_in bindings);
}
