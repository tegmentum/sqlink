//! Embed path for lorem. All FFI glue is in `sqlite-embed`; this
//! is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

const FID_WORDS: u64 = 1;
const FID_SENTENCES: u64 = 2;
const FID_TITLE: u64 = 3;
const FID_WORDS_SEEDED: u64 = 4;
const FID_SENTENCES_SEEDED: u64 = 5;

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

fn clamp_count(n: i64) -> usize {
    if n < 0 {
        0
    } else if n > 100_000 {
        100_000
    } else {
        n as usize
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_WORDS => {
            let n = clamp_count(arg_int(&args, 0, "lorem_words")?);
            Ok(SqlValueOwned::Text(lipsum::lipsum_words(n)))
        }
        FID_SENTENCES => {
            let n = clamp_count(arg_int(&args, 0, "lorem_sentences")?);
            Ok(SqlValueOwned::Text(lipsum::lipsum(n)))
        }
        FID_TITLE => Ok(SqlValueOwned::Text(lipsum::lipsum_title())),
        FID_WORDS_SEEDED => {
            let n = clamp_count(arg_int(&args, 0, "lorem_words_seeded")?);
            let seed = arg_int(&args, 1, "lorem_words_seeded")? as u64;
            let rng = ChaCha20Rng::seed_from_u64(seed);
            Ok(SqlValueOwned::Text(lipsum::lipsum_words_with_rng(rng, n)))
        }
        FID_SENTENCES_SEEDED => {
            let n = clamp_count(arg_int(&args, 0, "lorem_sentences_seeded")?);
            let seed = arg_int(&args, 1, "lorem_sentences_seeded")? as u64;
            let rng = ChaCha20Rng::seed_from_u64(seed);
            Ok(SqlValueOwned::Text(lipsum::lipsum_with_rng(rng, n)))
        }
        other => Err(format!("lorem: unknown func id {other}")),
    }
}

// Unseeded variants reseed per call  non-deterministic. Seeded
// variants ARE deterministic but share the same FID family; to
// keep behavior aligned with the WASM path (which marks the whole
// extension non-det at the SQL layer to avoid SQLite caching
// surprises across rows), we keep deterministic=false here too.
const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_WORDS,            name: b"lorem_words\0",            num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_SENTENCES,        name: b"lorem_sentences\0",        num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_TITLE,            name: b"lorem_title\0",            num_args: 0, deterministic: false },
    ScalarSpec { func_id: FID_WORDS_SEEDED,     name: b"lorem_words_seeded\0",     num_args: 2, deterministic: true  },
    ScalarSpec { func_id: FID_SENTENCES_SEEDED, name: b"lorem_sentences_seeded\0", num_args: 2, deterministic: true  },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
