//! `iso-639-5` extension  ISO 639-5 language family / collection
//! codes. Companion to the `iso-codes` extension (which covers the
//! per-language standards 639-1 / -2 / -3).
//!
//! Function surface:
//!
//!   iso639_5_name(code)      -> text     family English name, or NULL
//!   iso639_5_is_valid(code)  -> integer  0/1
//!   iso639_5_list()          -> text     JSON array of {code, name}
//!   iso639_5_version()       -> text     extension + standard year
//!
//! Lookups are case-insensitive on input; the canonical form on
//! output is lowercase per the ISO 639 convention. Unknown code
//! NULL (not an error). NULL input  NULL output.
//!
//! The table is the full official ISO 639-5 set (115 entries) as
//! published by the Library of Congress registrar. ISO has not
//! added a code since the 2013 update; the list is treated as
//! static.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
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

    const FID_NAME: u64 = 10;
    const FID_IS_VALID: u64 = 11;
    const FID_LIST: u64 = 12;
    const FID_VERSION: u64 = 99;

    struct Ext;

    /// Static ISO 639-5 table: (alpha-3 lowercase code, English name).
    /// Sourced from the Library of Congress 639-5 registry. 115 rows
    /// (the standard's full published set). Ordered alphabetically by
    /// code so a `_list()` consumer gets a predictable iteration order
    /// without needing a runtime sort.
    const ISO_639_5: &[(&str, &str)] = &[
        ("aav", "Austro-Asiatic languages"),
        ("afa", "Afro-Asiatic languages"),
        ("alg", "Algonquian languages"),
        ("alv", "Atlantic-Congo languages"),
        ("apa", "Apache languages"),
        ("aqa", "Alacalufan languages"),
        ("aql", "Algic languages"),
        ("art", "Artificial languages"),
        ("ath", "Athapascan languages"),
        ("auf", "Arauan languages"),
        ("aus", "Australian languages"),
        ("awd", "Arawakan languages"),
        ("azc", "Uto-Aztecan languages"),
        ("bad", "Banda languages"),
        ("bai", "Bamileke languages"),
        ("bat", "Baltic languages"),
        ("ber", "Berber languages"),
        ("bih", "Bihari languages"),
        ("bnt", "Bantu languages"),
        ("btk", "Batak languages"),
        ("cai", "Central American Indian languages"),
        ("cau", "Caucasian languages"),
        ("cba", "Chibchan languages"),
        ("ccn", "North Caucasian languages"),
        ("ccs", "South Caucasian languages"),
        ("cdc", "Chadic languages"),
        ("cdd", "Caddoan languages"),
        ("cel", "Celtic languages"),
        ("cmc", "Chamic languages"),
        ("cpe", "English-based creoles and pidgins"),
        ("cpf", "French-based creoles and pidgins"),
        ("cpp", "Portuguese-based creoles and pidgins"),
        ("crp", "Creoles and pidgins"),
        ("csu", "Central Sudanic languages"),
        ("cus", "Cushitic languages"),
        ("day", "Land Dayak languages"),
        ("dmn", "Mande languages"),
        ("dra", "Dravidian languages"),
        ("egx", "Egyptian languages"),
        ("esx", "Eskimo-Aleut languages"),
        ("euq", "Basque (family)"),
        ("fiu", "Finno-Ugrian languages"),
        ("fox", "Formosan languages"),
        ("gem", "Germanic languages"),
        ("gme", "East Germanic languages"),
        ("gmq", "North Germanic languages"),
        ("gmw", "West Germanic languages"),
        ("grk", "Greek languages"),
        ("hmx", "Hmong-Mien languages"),
        ("hok", "Hokan languages"),
        ("hyx", "Armenian (family)"),
        ("iir", "Indo-Iranian languages"),
        ("ijo", "Ijo languages"),
        ("inc", "Indic languages"),
        ("ine", "Indo-European languages"),
        ("ira", "Iranian languages"),
        ("iro", "Iroquoian languages"),
        ("itc", "Italic languages"),
        ("jpx", "Japanese (family)"),
        ("kar", "Karen languages"),
        ("kdo", "Kordofanian languages"),
        ("khi", "Khoisan languages"),
        ("kro", "Kru languages"),
        ("map", "Austronesian languages"),
        ("mkh", "Mon-Khmer languages"),
        ("mno", "Manobo languages"),
        ("mun", "Munda languages"),
        ("myn", "Mayan languages"),
        ("nah", "Nahuatl languages"),
        ("nai", "North American Indian languages"),
        ("ngf", "Trans-New Guinea languages"),
        ("nic", "Niger-Kordofanian languages"),
        ("nub", "Nubian languages"),
        ("omq", "Oto-Manguean languages"),
        ("omv", "Omotic languages"),
        ("oto", "Otomian languages"),
        ("paa", "Papuan languages"),
        ("phi", "Philippine languages"),
        ("plf", "Central Malayo-Polynesian languages"),
        ("poz", "Malayo-Polynesian languages"),
        ("pqe", "Eastern Malayo-Polynesian languages"),
        ("pqw", "Western Malayo-Polynesian languages"),
        ("pra", "Prakrit languages"),
        ("qwe", "Quechuan (family)"),
        ("roa", "Romance languages"),
        ("sai", "South American Indian languages"),
        ("sal", "Salishan languages"),
        ("sdv", "Eastern Sudanic languages"),
        ("sem", "Semitic languages"),
        ("sgn", "Sign languages"),
        ("sio", "Siouan languages"),
        ("sit", "Sino-Tibetan languages"),
        ("sla", "Slavic languages"),
        ("smi", "Sami languages"),
        ("son", "Songhai languages"),
        ("sqj", "Albanian languages"),
        ("ssa", "Nilo-Saharan languages"),
        ("syd", "Samoyedic languages"),
        ("tai", "Tai languages"),
        ("tbq", "Tibeto-Burman languages"),
        ("trk", "Turkic languages"),
        ("tup", "Tupi languages"),
        ("tut", "Altaic languages"),
        ("tuw", "Tungus languages"),
        ("urj", "Uralic languages"),
        ("wak", "Wakashan languages"),
        ("wen", "Sorbian languages"),
        ("xgn", "Mongolian languages"),
        ("xnd", "Na-Dene languages"),
        ("ypk", "Yupik languages"),
        ("zhx", "Chinese (family)"),
        ("zle", "East Slavic languages"),
        ("zls", "South Slavic languages"),
        ("zlw", "West Slavic languages"),
        ("znd", "Zande languages"),
    ];

    /// Case-insensitive lookup. Trims surrounding whitespace so a
    /// caller piping CSV cells (`'  sla  '`) doesn't see surprise
    /// NULLs. Returns the English name on hit.
    fn lookup(code: &str) -> Option<&'static str> {
        let lc = code.trim().to_ascii_lowercase();
        if lc.len() != 3 {
            return None;
        }
        // Linear scan: 115 rows is small enough that the cost is below
        // any hash-table overhead, and it keeps the static table free
        // of `phf` / build-script dependencies.
        for (k, v) in ISO_639_5 {
            if *k == lc {
                return Some(*v);
            }
        }
        None
    }

    /// Pull a TEXT arg. NULL is signaled by `None` so the caller can
    /// short-circuit to NULL (NULL  NULL semantics). Non-TEXT (and
    /// non-NULL) is an error so a silently-coerced INTEGER doesn't
    /// masquerade as a code.
    fn arg_text_opt(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(SqlValue::Null) | None => Ok(None),
            _ => Err(format!("{fname}: arg {i} must be TEXT")),
        }
    }

    /// Hand-rolled JSON escape for the list output. The table contains
    /// only ASCII letters, spaces, and hyphens (verified by eye on the
    /// 115-row table), so the only character we strictly need to
    /// escape is `"`. We still escape `\\` and the C0 control range
    /// defensively in case the table grows.
    fn json_escape(s: &str, out: &mut String) {
        for ch in s.chars() {
            match ch {
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
    }

    fn list_json() -> String {
        // Pre-size: each row is ~ {"code":"xxx","name":"..."} . 64
        // bytes per row is a reasonable upper bound for the existing
        // table; if a future row is longer the String will just grow.
        let mut s = String::with_capacity(64 * ISO_639_5.len() + 2);
        s.push('[');
        for (i, (code, name)) in ISO_639_5.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str("{\"code\":\"");
            json_escape(code, &mut s);
            s.push_str("\",\"name\":\"");
            json_escape(name, &mut s);
            s.push_str("\"}");
        }
        s.push(']');
        s
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
                name: "iso-639-5".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NAME, "iso639_5_name", 1),
                    s(FID_IS_VALID, "iso639_5_is_valid", 1),
                    s(FID_LIST, "iso639_5_list", 0),
                    s(FID_VERSION, "iso639_5_version", 0),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_NAME => {
                    let t = match arg_text_opt(&args, 0, "iso639_5_name")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match lookup(&t) {
                        Some(n) => SqlValue::Text(n.to_string()),
                        None => SqlValue::Null,
                    })
                }
                FID_IS_VALID => {
                    // is_valid on NULL returns NULL (3VL): the caller
                    // can't have intended a yes/no answer for an
                    // absent code, and propagating NULL keeps `WHERE
                    // iso639_5_is_valid(c)` filtering NULLs out
                    // naturally.
                    let t = match arg_text_opt(&args, 0, "iso639_5_is_valid")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Integer(if lookup(&t).is_some() { 1 } else { 0 }))
                }
                FID_LIST => Ok(SqlValue::Text(list_json())),
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "iso-639-5 {}; ISO 639-5:2008+2013 (115 codes)",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("iso-639-5: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
