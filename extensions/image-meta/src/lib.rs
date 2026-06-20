//! Image header metadata: format / width / height / dimensions from
//! a blob's header bytes, via the `imagesize` 0.13 pure-rust
//! header-only decoder.
//!
//! Use case: photo databases, image-aware vtab paths, anywhere you
//! have BLOBs and want to query their pixel dimensions or detect
//! the format without decoding pixel data.
//!
//! Function surface (PLAN-more-extensions-2.md  7):
//!
//!   img_format(blob)      -> TEXT
//!       One of: 'PNG' | 'JPEG' | 'GIF' | 'WebP' | 'BMP' | 'TIFF'
//!       | 'AVIF' | 'HEIC' | 'ICO' | 'PSD' | 'DDS' | 'EXR' | 'HDR'
//!       | 'TGA' | 'PNM' | 'QOI' | 'JXL' | 'KTX2' | 'ASEPRITE'
//!       | 'FARBFELD' | 'ILBM' | 'VTF'
//!   img_width(blob)       -> INTEGER
//!   img_height(blob)      -> INTEGER
//!   img_dimensions(blob)  -> TEXT  (JSON: {"width":N,"height":N,"format":"PNG"})
//!   img_byte_size(blob)   -> INTEGER  (length(blob); convenience)
//!   img_version()         -> TEXT
//!
//! NULL or unrecognized input  NULL on every fn (NEVER an error).
//! Reads ONLY the header, not full pixel data; works on partial
//! reads (e.g. the first 4 KB of a TIFF).
//!
//! HEIF/AVIF support: imagesize 0.13 recognizes the HEIF container
//! (`ftyp` box) and distinguishes the compression family. We
//! report 'AVIF' for HEIF/AV1 streams and 'HEIC' for HEIF/HEVC or
//! HEIF/JPEG. Width/height extraction goes through the `meta`/`ipco`
//! `ispe` box  works on the bulk of HEIF blobs in the wild, but
//! some encoders nest `meta` deeper than imagesize walks. Truncated
//! HEIF containers may return a format but NULL dimensions.

extern crate alloc;

/// Map an imagesize ImageType to the format string we expose to SQL.
///
/// HEIF distinguishes inner compression  AV1 -> AVIF, HEVC/JPEG ->
/// HEIC (HEIC is the Apple HEVC-in-HEIF tradename). Unknown HEIF
/// brands fall back to 'HEIC' as the closest match  callers
/// scanning a photo library overwhelmingly want "is this a modern
/// container format" and that single label is the actionable
/// answer.
#[cfg(target_arch = "wasm32")]
fn image_type_label(t: imagesize::ImageType) -> &'static str {
    use imagesize::{Compression, ImageType};
    match t {
        ImageType::Png => "PNG",
        ImageType::Jpeg => "JPEG",
        ImageType::Gif => "GIF",
        ImageType::Webp => "WebP",
        ImageType::Bmp => "BMP",
        ImageType::Tiff => "TIFF",
        ImageType::Heif(Compression::Av1) => "AVIF",
        ImageType::Heif(Compression::Hevc) => "HEIC",
        ImageType::Heif(Compression::Jpeg) => "HEIC",
        ImageType::Heif(Compression::Unknown) => "HEIC",
        ImageType::Ico => "ICO",
        ImageType::Psd => "PSD",
        ImageType::Dds => "DDS",
        ImageType::Exr => "EXR",
        ImageType::Hdr => "HDR",
        ImageType::Tga => "TGA",
        ImageType::Pnm => "PNM",
        ImageType::Qoi => "QOI",
        ImageType::Jxl => "JXL",
        ImageType::Ktx2 => "KTX2",
        ImageType::Aseprite => "ASEPRITE",
        ImageType::Farbfeld => "FARBFELD",
        ImageType::Ilbm => "ILBM",
        ImageType::Vtf => "VTF",
        // imagesize::ImageType is marked #[non_exhaustive]; future
        // variants land as a generic "OTHER" label instead of
        // breaking the build. Bump the crate + add an explicit
        // mapping when you upgrade.
        _ => "OTHER",
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use super::image_type_label;
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

    // ---- Function IDs ----
    const FID_FORMAT: u64 = 1;
    const FID_WIDTH: u64 = 2;
    const FID_HEIGHT: u64 = 3;
    const FID_DIMENSIONS: u64 = 4;
    const FID_BYTE_SIZE: u64 = 5;
    const FID_VERSION: u64 = 6;

    struct Ext;

    /// Pull bytes out of arg 0. Returns:
    ///   - Some(bytes) for BLOB / TEXT (text becomes its raw UTF-8)
    ///   - None for NULL or any other unexpected type
    ///
    /// "Any unexpected type -> None" is intentional: per the plan
    /// the *_format / *_width / *_height / *_dimensions surface
    /// must NEVER raise on bad input  it just returns SQL NULL.
    fn opt_bytes(args: &[SqlValue]) -> Option<Vec<u8>> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// Try to decode dimensions; never panics, never returns Err to
    /// the caller. imagesize::blob_size returns Err for unsupported
    /// formats or insufficient data  both map to None here.
    fn try_dims(bytes: &[u8]) -> Option<imagesize::ImageSize> {
        imagesize::blob_size(bytes).ok()
    }

    /// Try to identify the format; same NULL-on-fail contract.
    fn try_format(bytes: &[u8]) -> Option<imagesize::ImageType> {
        imagesize::image_type(bytes).ok()
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Pure functions of the input blob  fully deterministic.
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "image_meta".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FORMAT, "img_format", 1, det),
                    s(FID_WIDTH, "img_width", 1, det),
                    s(FID_HEIGHT, "img_height", 1, det),
                    s(FID_DIMENSIONS, "img_dimensions", 1, det),
                    s(FID_BYTE_SIZE, "img_byte_size", 1, det),
                    s(FID_VERSION, "img_version", 0, det),
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
                FID_FORMAT => {
                    let Some(bytes) = opt_bytes(&args) else {
                        return Ok(SqlValue::Null);
                    };
                    match try_format(&bytes) {
                        Some(t) => Ok(SqlValue::Text(image_type_label(t).to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_WIDTH => {
                    let Some(bytes) = opt_bytes(&args) else {
                        return Ok(SqlValue::Null);
                    };
                    match try_dims(&bytes) {
                        Some(d) => Ok(SqlValue::Integer(d.width as i64)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_HEIGHT => {
                    let Some(bytes) = opt_bytes(&args) else {
                        return Ok(SqlValue::Null);
                    };
                    match try_dims(&bytes) {
                        Some(d) => Ok(SqlValue::Integer(d.height as i64)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_DIMENSIONS => {
                    let Some(bytes) = opt_bytes(&args) else {
                        return Ok(SqlValue::Null);
                    };
                    // Need BOTH a recognized format and decodable
                    // dimensions. If we can identify the format but
                    // not the dimensions (truncated JPEG SOI etc.),
                    // we still return NULL  the plan calls for a
                    // single JSON object describing a fully-decoded
                    // image, and json_extract on a NULL is safe.
                    let (Some(t), Some(d)) = (try_format(&bytes), try_dims(&bytes)) else {
                        return Ok(SqlValue::Null);
                    };
                    let json = format!(
                        "{{\"width\":{},\"height\":{},\"format\":\"{}\"}}",
                        d.width,
                        d.height,
                        image_type_label(t),
                    );
                    Ok(SqlValue::Text(json))
                }
                FID_BYTE_SIZE => {
                    // length() convenience  reflects the input we
                    // saw, including for TEXT (raw UTF-8 byte count).
                    // NULL passes through.
                    match args.first() {
                        Some(SqlValue::Blob(b)) => Ok(SqlValue::Integer(b.len() as i64)),
                        Some(SqlValue::Text(s)) => Ok(SqlValue::Integer(s.len() as i64)),
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("image_meta: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
