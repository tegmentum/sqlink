//! Embed path for morse. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

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

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let t = arg_text(&args, 0, "morse")?;
    match func_id {
        FID_ENCODE => Ok(SqlValueOwned::Text(encode(&t))),
        FID_DECODE => Ok(SqlValueOwned::Text(decode(&t))),
        other => Err(format!("morse: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_ENCODE, name: b"morse_encode\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_DECODE, name: b"morse_decode\0", num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
