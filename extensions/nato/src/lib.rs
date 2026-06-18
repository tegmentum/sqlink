//! NATO phonetic alphabet (A->Alpha, B->Bravo)

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

    const FID_ENCODE: u64 = 1;
    const FID_DECODE: u64 = 2;
    const FID_WORD: u64 = 3;

    struct Ext;

    /// (letter, NATO phonetic word).
    /// Letters A-Z + digits 0-9; the official ICAO/NATO assignments.
    const TABLE: &[(char, &str)] = &[
        ('A', "Alpha"),   ('B', "Bravo"),    ('C', "Charlie"),
        ('D', "Delta"),   ('E', "Echo"),     ('F', "Foxtrot"),
        ('G', "Golf"),    ('H', "Hotel"),    ('I', "India"),
        ('J', "Juliet"),  ('K', "Kilo"),     ('L', "Lima"),
        ('M', "Mike"),    ('N', "November"), ('O', "Oscar"),
        ('P', "Papa"),    ('Q', "Quebec"),   ('R', "Romeo"),
        ('S', "Sierra"),  ('T', "Tango"),    ('U', "Uniform"),
        ('V', "Victor"),  ('W', "Whiskey"),  ('X', "X-ray"),
        ('Y', "Yankee"),  ('Z', "Zulu"),
        // Digits per ITU/ICAO: spelled out with phonetic variants.
        ('0', "Zero"),    ('1', "One"),      ('2', "Two"),
        ('3', "Three"),   ('4', "Four"),     ('5', "Five"),
        ('6', "Six"),     ('7', "Seven"),    ('8', "Eight"),
        ('9', "Nine"),
    ];

    fn word_for(c: char) -> Option<&'static str> {
        let upper = c.to_ascii_uppercase();
        TABLE.iter().find(|(k, _)| *k == upper).map(|(_, v)| *v)
    }

    /// Encode each alphanumeric character. Multi-word input gets
    /// " | " between words so decode can recover the spaces.
    /// Non-alphanumeric characters pass through verbatim, separated
    /// by spaces like the words.
    fn encode(s: &str) -> String {
        let mut parts: Vec<String> = alloc::vec![];
        for word in s.split_whitespace() {
            let mut letters: Vec<String> = alloc::vec![];
            for c in word.chars() {
                letters.push(match word_for(c) {
                    Some(w) => w.to_string(),
                    None => c.to_string(),  // passthrough (e.g. "-")
                });
            }
            parts.push(letters.join(" "));
        }
        parts.join(" | ")
    }

    /// Decode "Alpha Bravo Charlie" / "alpha bravo charlie"
    /// back to "ABC". "|" boundaries become spaces. Unknown
    /// words pass through as their first character (or '?').
    fn decode(s: &str) -> String {
        let mut out = String::new();
        let mut first_segment = true;
        for segment in s.split('|') {
            if !first_segment {
                out.push(' ');
            }
            first_segment = false;
            for w in segment.split_whitespace() {
                let key = w.to_ascii_lowercase();
                let matched = TABLE.iter().find(|(_, name)| {
                    name.to_ascii_lowercase() == key
                });
                match matched {
                    Some((c, _)) => out.push(*c),
                    None => {
                        // Unknown word  use first char if alphanumeric.
                        if let Some(c) = w.chars().next() {
                            if c.is_ascii_alphanumeric() {
                                out.push(c.to_ascii_uppercase());
                            } else {
                                out.push('?');
                            }
                        }
                    }
                }
            }
        }
        out
    }

    // ---- Arg helpers ----
    // The Big Three; copy-pasted into every extension. The
    // scaffold ships them so you delete what you don't need.

    #[allow(dead_code)]
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Available flags  pass `det` for deterministic scalars
            // (most cases), `nd` for ones that produce different
            // output each call (rng / time-of-call / counter).
            #[allow(unused_variables)]
            let det = FunctionFlags::DETERMINISTIC;
            #[allow(unused_variables)]
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "nato".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE, "nato_encode", 1, det),
                    s(FID_DECODE, "nato_decode", 1, det),
                    s(FID_WORD, "nato_word", 1, det),
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
            let raw = arg_text(&args, 0, "nato")?;
            match func_id {
                FID_ENCODE => Ok(SqlValue::Text(encode(&raw))),
                FID_DECODE => Ok(SqlValue::Text(decode(&raw))),
                FID_WORD => Ok(raw.chars().next()
                    .and_then(word_for)
                    .map(|w| SqlValue::Text(w.to_string()))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("nato: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
