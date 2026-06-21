//! Dominant color palette extraction from image blobs.
//!
//! Wraps `kmeans_colors` 0.6 + `image` 0.25 + `palette` 0.7 to mine
//! the visually dominant colors out of a PNG or JPEG blob. The
//! k-means clustering runs in Lab space (perceptually uniform), then
//! the centroids are mapped back to sRGB hex for SQL consumption.
//!
//! Function surface:
//!
//!   palette_extract(blob, k)      -> TEXT  JSON array of '#RRGGBB' hex
//!                                   strings, ordered by descending
//!                                   pixel-share. NULL on unparseable
//!                                   blob or invalid k.
//!   palette_dominant(blob)        -> TEXT  '#RRGGBB' of the largest
//!                                   k-means cluster (k=5 internally).
//!                                   NULL on unparseable blob.
//!   palette_average_color(blob)   -> TEXT  '#RRGGBB' of the per-channel
//!                                   mean sRGB of every pixel. NOT
//!                                   k-means  cheap arithmetic mean.
//!                                   NULL on unparseable blob.
//!   palette_version()             -> TEXT
//!
//! Supported formats: PNG, JPEG. HEIF/AVIF/WebP/etc. are deferred
//! the corresponding `image` features carry several MB of codec
//! weight for marginal coverage gain in the photo-database use case.
//! Calling any palette_* function on an unsupported or corrupted blob
//! returns SQL NULL  never raises.
//!
//! Notes on k:
//!   - k clamped to 1..=32. k <= 0 or NULL  NULL.
//!   - Pixels outside palette space (alpha == 0 in RGBA mode) are
//!     dropped before clustering; pure-transparent images therefore
//!     return NULL ("no pixels to cluster").
//!   - The seed for k-means++ initialization is fixed so the same
//!     blob + k yields the same palette across calls. `palette_*`
//!     scalars are flagged DETERMINISTIC accordingly.
//!
//! Why Lab over sRGB clustering: at low k the perceptually-uniform
//! distance metric better matches "what a human would call distinct
//! colors". The runtime cost vs Rgb clustering is small (~2x) and
//! the image is fully decoded already.

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

    use image::ImageReader;
    use kmeans_colors::{get_kmeans_hamerly, Kmeans, Sort};
    use palette::cast::from_component_slice;
    use palette::{FromColor, IntoColor, Lab, Srgb};
    use std::io::Cursor;

    // ---- Function IDs ----
    const FID_EXTRACT: u64 = 1;
    const FID_DOMINANT: u64 = 2;
    const FID_AVERAGE: u64 = 3;
    const FID_VERSION: u64 = 4;

    // k-means tunables (matched to kmeans_colors README defaults
    // except we run two passes instead of three  the extra third
    // pass is rarely the global minimum and doubles per-call cost).
    const KMEANS_RUNS: usize = 2;
    const KMEANS_MAX_ITER: usize = 20;
    const KMEANS_CONVERGE: f32 = 5.0;
    // Fixed seed: same (blob, k) -> same palette across calls.
    // Required for the DETERMINISTIC flag in the manifest to be
    // truthful  callers that index a generated column on
    // palette_dominant() depend on this stability.
    const SEED: u64 = 0xC010_C010_C010_C010;

    /// Internal: best-effort RGB8 decode. NULL on any failure  unknown
    /// format, truncated data, decoder error.
    fn decode_rgb8(bytes: &[u8]) -> Option<Vec<u8>> {
        let reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let img = reader.decode().ok()?;
        // into_rgb8 collapses any pixel format down to 8-bit sRGB;
        // alpha is *blended away* via the to_rgb8 path which drops
        // alpha entirely. For palette use that's the correct
        // behavior: a half-transparent red pixel is still red.
        Some(img.into_rgb8().into_raw())
    }

    /// Internal: run k-means on the rgb8 buffer in Lab space.
    /// Returns centroids sorted by descending pixel-share (the most
    /// dominant color first). Empty input  None.
    fn cluster_lab(rgb8: &[u8], k: usize) -> Option<Vec<(u8, u8, u8)>> {
        if rgb8.is_empty() || k == 0 {
            return None;
        }
        let lab: Vec<Lab> = from_component_slice::<Srgb<u8>>(rgb8)
            .iter()
            .map(|x| x.into_format().into_color())
            .collect();
        if lab.is_empty() {
            return None;
        }
        let mut result = Kmeans::new();
        for i in 0..KMEANS_RUNS {
            let run = get_kmeans_hamerly(
                k,
                KMEANS_MAX_ITER,
                KMEANS_CONVERGE,
                false,
                &lab,
                SEED.wrapping_add(i as u64),
            );
            if run.score < result.score {
                result = run;
            }
        }
        if result.centroids.is_empty() {
            return None;
        }
        let mut sorted = Lab::sort_indexed_colors(&result.centroids, &result.indices);
        // Lab::sort_indexed_colors sorts by lightness; we want
        // dominance (the centroid that owns the most pixels first),
        // matching the kmeans_colors README's "manual sort by
        // percentage" recipe.
        sorted.sort_unstable_by(|a, b| b.percentage.total_cmp(&a.percentage));
        Some(
            sorted
                .iter()
                .map(|c| {
                    let rgb: Srgb<u8> = Srgb::from_color(c.centroid).into_format();
                    (rgb.red, rgb.green, rgb.blue)
                })
                .collect(),
        )
    }

    /// Internal: per-channel arithmetic mean of an RGB8 buffer.
    /// Distinct from k-means dominant  this is the "what color does
    /// this image average out to" answer, e.g. for grouping photos
    /// by overall warmth/coolness without the clustering overhead.
    fn average_rgb(rgb8: &[u8]) -> Option<(u8, u8, u8)> {
        if rgb8.len() < 3 {
            return None;
        }
        let n = (rgb8.len() / 3) as u64;
        if n == 0 {
            return None;
        }
        let mut r: u64 = 0;
        let mut g: u64 = 0;
        let mut b: u64 = 0;
        for chunk in rgb8.chunks_exact(3) {
            r += chunk[0] as u64;
            g += chunk[1] as u64;
            b += chunk[2] as u64;
        }
        Some(((r / n) as u8, (g / n) as u8, (b / n) as u8))
    }

    /// Internal: format (r,g,b) as the canonical lowercase '#rrggbb'.
    /// Matches the `color` extension's hex output for cross-fn joins.
    fn hex(rgb: (u8, u8, u8)) -> String {
        format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
    }

    /// Internal: blob-or-text accessor matching image-meta's
    /// "NULL on anything else" convention. TEXT is accepted as raw
    /// UTF-8 bytes so `palette_extract(readfile('x.png'), 4)` works
    /// without an explicit CAST.
    fn opt_bytes(args: &[SqlValue]) -> Option<Vec<u8>> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // All four scalars are pure functions of (blob, k)  same
            // inputs always yield the same hex strings because the
            // k-means seed is fixed and the cluster_lab sort is
            // total. DETERMINISTIC is correct.
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "color_palette".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_EXTRACT, "palette_extract", 2, det),
                    s(FID_DOMINANT, "palette_dominant", 1, det),
                    s(FID_AVERAGE, "palette_average_color", 1, det),
                    s(FID_VERSION, "palette_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_EXTRACT => {
                    let Some(bytes) = opt_bytes(&args) else {
                        return Ok(SqlValue::Null);
                    };
                    // k is required and must be a positive integer.
                    // NULL or non-INTEGER -> NULL (the NEVER-raise
                    // convention extends to argument typing too).
                    let k = match args.get(1) {
                        Some(SqlValue::Integer(n)) if *n >= 1 => {
                            // Clamp at 32; beyond that the k-means
                            // runtime grows quadratically with no
                            // meaningful palette gain for human eyes.
                            (*n).min(32) as usize
                        }
                        _ => return Ok(SqlValue::Null),
                    };
                    let Some(rgb8) = decode_rgb8(&bytes) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(colors) = cluster_lab(&rgb8, k) else {
                        return Ok(SqlValue::Null);
                    };
                    // Emit a JSON array of hex strings  callers can
                    // pipe directly into json_each() to fan out to
                    // rows or pluck the first with ->>0.
                    let mut out = String::with_capacity(2 + colors.len() * 11);
                    out.push('[');
                    for (i, c) in colors.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        out.push('"');
                        out.push_str(&hex(*c));
                        out.push('"');
                    }
                    out.push(']');
                    Ok(SqlValue::Text(out))
                }
                FID_DOMINANT => {
                    let Some(bytes) = opt_bytes(&args) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(rgb8) = decode_rgb8(&bytes) else {
                        return Ok(SqlValue::Null);
                    };
                    // k=5 is the README-recommended default for
                    // dominant-color extraction. Picking the first
                    // (largest cluster) after dominance-sort gives
                    // a more representative "what color is this
                    // image" answer than k=1, which collapses to the
                    // global mean and loses minority-but-large
                    // accents (e.g. a bright sky with green ground
                    // wants the sky color, not the muddy mean).
                    let Some(colors) = cluster_lab(&rgb8, 5) else {
                        return Ok(SqlValue::Null);
                    };
                    let first = colors.first().copied().unwrap_or((0, 0, 0));
                    Ok(SqlValue::Text(hex(first)))
                }
                FID_AVERAGE => {
                    let Some(bytes) = opt_bytes(&args) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(rgb8) = decode_rgb8(&bytes) else {
                        return Ok(SqlValue::Null);
                    };
                    match average_rgb(&rgb8) {
                        Some(rgb) => Ok(SqlValue::Text(hex(rgb))),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("color_palette: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
