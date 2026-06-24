//! Compression scalars over the compression-multiplexer
//! providers. Wire format: 1-byte algorithm tag + 4-byte
//! little-endian original length + algorithm-specific payload.
//! The tag lets `decompress(b)` route to the right decoder
//! without a second argument.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use compression_multiplexer::providers::{
    algorithm_description, get_provider, supported_algorithms, Algorithm,
};

#[cfg(feature = "embed")]
pub mod embed;

/// 1-byte tag mapping; mirrors the Algorithm enum but as a u8
/// so it fits in the blob header. Compatible with future
/// expansion (we'd add new variants here and bump
/// FORMAT_VERSION below).
fn algo_to_tag(a: Algorithm) -> u8 {
    match a {
        Algorithm::Store => 0,
        Algorithm::Deflate => 1,
        Algorithm::Bzip2 => 2,
        Algorithm::Lzma => 3,
        Algorithm::Zstd => 4,
        Algorithm::Lz4 => 5,
        Algorithm::Openzl => 6,
    }
}

fn tag_to_algo(tag: u8) -> Option<Algorithm> {
    match tag {
        0 => Some(Algorithm::Store),
        1 => Some(Algorithm::Deflate),
        2 => Some(Algorithm::Bzip2),
        3 => Some(Algorithm::Lzma),
        4 => Some(Algorithm::Zstd),
        5 => Some(Algorithm::Lz4),
        6 => Some(Algorithm::Openzl),
        _ => None,
    }
}

fn parse_algo(name: &str) -> Result<Algorithm, String> {
    match name.to_ascii_lowercase().as_str() {
        "store" | "none" => Ok(Algorithm::Store),
        "deflate" | "zlib" => Ok(Algorithm::Deflate),
        "bzip2" | "bz2" => Ok(Algorithm::Bzip2),
        "lzma" | "xz" => Ok(Algorithm::Lzma),
        "lz4" => Ok(Algorithm::Lz4),
        "zstd" => Ok(Algorithm::Zstd),
        "openzl" => Ok(Algorithm::Openzl),
        other => Err(format!("unknown algorithm '{other}'")),
    }
}

pub fn compress(input: &[u8], algo_name: &str, level: u8) -> Result<Vec<u8>, String> {
    let algo = parse_algo(algo_name)?;
    let provider = get_provider(algo)?;
    let body = provider.compress(input, level)?;
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(algo_to_tag(algo));
    out.extend_from_slice(&(input.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn decompress(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() < 5 {
        return Err("compress: input too short for tag + length header".to_string());
    }
    let tag = input[0];
    let algo = tag_to_algo(tag).ok_or_else(|| format!("unknown algorithm tag {tag}"))?;
    let provider = get_provider(algo)?;
    provider.decompress(&input[5..])
}

pub fn list_algorithms() -> String {
    // Owned-String form so the iterator's lifetime is bounded.
    let names: Vec<String> = supported_algorithms()
        .iter()
        .filter_map(|a| algorithm_description(*a))
        .map(|s| {
            // Pull the algorithm short-name out of "DEFLATE: ..."
            // descriptions; the caller wants the SQL-accepted
            // token, not the prose. Fall back to the full string
            // if there's no colon.
            let leading = s.split(':').next().unwrap_or(s.as_str()).trim();
            leading.to_string()
        })
        .collect();
    serde_json_minimal(&names)
}

/// One-shot JSON list serializer. Algorithm names are simple
/// alphanumeric tokens (no quotes, backslashes, control chars),
/// so a minimal emitter that doesn't escape is correct here.
fn serde_json_minimal(items: &[String]) -> String {
    let mut out = String::with_capacity(items.iter().map(|s| s.len() + 4).sum::<usize>() + 2);
    out.push('[');
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(s);
        out.push('"');
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(algo: &str, payload: &[u8]) {
        let c = compress(payload, algo, 6).unwrap();
        let d = decompress(&c).unwrap();
        assert_eq!(d, payload, "round-trip failed for {algo}");
    }

    #[test]
    fn store_round_trip() {
        round_trip("store", b"hello world");
    }

    #[test]
    fn deflate_round_trip() {
        round_trip("deflate", &b"abcabcabcabc".repeat(50));
    }

    #[test]
    fn lz4_round_trip() {
        round_trip("lz4", &b"the quick brown fox ".repeat(20));
    }

    #[test]
    fn bzip2_round_trip() {
        round_trip("bzip2", &b"xyzxyzxyz".repeat(40));
    }

    #[test]
    fn unknown_algo_errors() {
        assert!(compress(b"x", "snappy", 6).is_err());
    }

    #[test]
    fn truncated_input_errors() {
        assert!(decompress(&[1, 2, 3]).is_err());
    }
}

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

    const FID_COMPRESS_2: u64 = 1;
    const FID_COMPRESS_3: u64 = 2;
    const FID_DECOMPRESS: u64 = 3;
    const FID_VERSION: u64 = 4;
    const FID_ALGORITHMS: u64 = 5;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, num_args: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: f,
            };
            Manifest {
                name: "compress".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_COMPRESS_2, "compress", 2, det),
                    // 3-arg form takes explicit `level` (6 default).
                    s(FID_COMPRESS_3, "compress", 3, det),
                    s(FID_DECOMPRESS, "decompress", 1, det),
                    s(FID_VERSION, "compress_version", 0, nd),
                    s(FID_ALGORITHMS, "compress_algorithms", 0, nd),
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

    fn arg_bytes<'a>(args: &'a [SqlValue], i: usize, fname: &str) -> Result<&'a [u8], String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes()),
            Some(SqlValue::Null) | None => Err(format!("{fname}: null arg at {i}")),
            _ => Err(format!("{fname}: BLOB or TEXT arg at {i}")),
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_level(args: &[SqlValue]) -> u8 {
        match args.get(2) {
            Some(SqlValue::Integer(n)) => (*n).clamp(0, 22) as u8,
            _ => 6,
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_ALGORITHMS => Ok(SqlValue::Text(super::list_algorithms())),
                FID_COMPRESS_2 | FID_COMPRESS_3 => {
                    let input = arg_bytes(&args, 0, "compress")?;
                    let algo = arg_text(&args, 1, "compress")?;
                    let level = if func_id == FID_COMPRESS_3 {
                        arg_level(&args)
                    } else {
                        6
                    };
                    super::compress(input, &algo, level)
                        .map(SqlValue::Blob)
                        .map_err(|e| format!("compress: {e}"))
                }
                FID_DECOMPRESS => {
                    let input = arg_bytes(&args, 0, "decompress")?;
                    super::decompress(input)
                        .map(SqlValue::Blob)
                        .map_err(|e| format!("decompress: {e}"))
                }
                other => Err(format!("compress: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
