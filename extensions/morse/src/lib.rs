//! Morse code encode/decode.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

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

    const TABLE: &[(char, &str)] = &[
        ('A', ".-"),
        ('B', "-..."),
        ('C', "-.-."),
        ('D', "-.."),
        ('E', "."),
        ('F', "..-."),
        ('G', "--."),
        ('H', "...."),
        ('I', ".."),
        ('J', ".---"),
        ('K', "-.-"),
        ('L', ".-.."),
        ('M', "--"),
        ('N', "-."),
        ('O', "---"),
        ('P', ".--."),
        ('Q', "--.-"),
        ('R', ".-."),
        ('S', "..."),
        ('T', "-"),
        ('U', "..-"),
        ('V', "...-"),
        ('W', ".--"),
        ('X', "-..-"),
        ('Y', "-.--"),
        ('Z', "--.."),
        ('0', "-----"),
        ('1', ".----"),
        ('2', "..---"),
        ('3', "...--"),
        ('4', "....-"),
        ('5', "....."),
        ('6', "-...."),
        ('7', "--..."),
        ('8', "---.."),
        ('9', "----."),
        ('.', ".-.-.-"),
        (',', "--..--"),
        ('?', "..--.."),
        ('\'', ".----."),
        ('!', "-.-.--"),
        ('/', "-..-."),
        ('(', "-.--."),
        (')', "-.--.-"),
        ('&', ".-..."),
        (':', "---..."),
        (';', "-.-.-."),
        ('=', "-...-"),
        ('+', ".-.-."),
        ('-', "-....-"),
        ('_', "..--.-"),
        ('"', ".-..-."),
        ('$', "...-..-"),
        ('@', ".--.-."),
    ];

    fn encode_char(c: char) -> &'static str {
        let upper = c.to_ascii_uppercase();
        for (k, v) in TABLE {
            if *k == upper {
                return v;
            }
        }
        "?"
    }

    fn decode_token(t: &str) -> Option<char> {
        let norm: String = t
            .chars()
            .map(|c| match c {
                '*' => '.',
                '_' => '-',
                _ => c,
            })
            .collect();
        for (k, v) in TABLE {
            if *v == norm {
                return Some(*k);
            }
        }
        None
    }

    fn encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len() * 4);
        let mut words = s.split_whitespace().peekable();
        while let Some(w) = words.next() {
            let mut letters = w.chars().peekable();
            while let Some(c) = letters.next() {
                out.push_str(encode_char(c));
                if letters.peek().is_some() {
                    out.push(' ');
                }
            }
            if words.peek().is_some() {
                out.push_str(" / ");
            }
        }
        out
    }

    fn decode(s: &str) -> String {
        let mut out = String::new();
        let mut words = s.split(" / ").peekable();
        while let Some(w) = words.next() {
            for tok in w.split_whitespace() {
                if let Some(c) = decode_token(tok) {
                    out.push(c);
                } else {
                    out.push('?');
                }
            }
            if words.peek().is_some() {
                out.push(' ');
            }
        }
        out
    }

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
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
                name: "morse".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE, "morse_encode", 1),
                    s(FID_DECODE, "morse_decode", 1),
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
            let t = arg_text(&args, 0, "morse")?;
            match func_id {
                FID_ENCODE => Ok(SqlValue::Text(encode(&t))),
                FID_DECODE => Ok(SqlValue::Text(decode(&t))),
                other => Err(format!("morse: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
