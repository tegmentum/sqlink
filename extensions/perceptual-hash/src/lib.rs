//! Perceptual image hashing  pHash / dHash / aHash for image similarity.
//!
//! Function surface:
//!   phash(blob)                 -> BLOB(8)   8x8 DCT-based perceptual hash
//!   dhash(blob)                 -> BLOB(8)   9x8 horizontal-diff hash
//!   ahash(blob)                 -> BLOB(8)   8x8 mean-threshold hash
//!   hash_distance(blob, blob)   -> INTEGER   Hamming distance (popcount of XOR)
//!   perceptual_hash_version()   -> TEXT
//!
//! Decoders: png 0.17 + jpeg-decoder 0.3 only. Other formats -> NULL.
//! Unparseable / NULL input -> NULL on every fn. Never raises.
//!
//! Hash algorithms (well-known forms; see Neal Krawetz's hackerfactor
//! posts):
//!   aHash  resize to 8x8 grayscale, threshold each pixel against the
//!          mean; bit=1 if pixel >= mean.
//!   dHash  resize to 9x8 grayscale, for each row compare adjacent
//!          pixels left-to-right; bit=1 if left > right. 8 rows * 8
//!          comparisons = 64 bits.
//!   pHash  resize to 32x32 grayscale, run 2D DCT-II, take the top-left
//!          8x8 block excluding the DC coefficient (0,0). Threshold
//!          against the median of those 63 AC coefficients; bit=1 if
//!          coeff > median. 64 bits total (we keep position (0,0) bit
//!          as the median-vs-median result, conventionally 0).
//!
//! Bit ordering: bit i (from MSB) of byte i/8 corresponds to pixel/coef
//! index i in row-major order. So byte 0 holds the first 8 hash bits,
//! MSB-first. This matches the de-facto img_hash + ImageHash Python
//! library convention.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
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
    const FID_PHASH: u64 = 1;
    const FID_DHASH: u64 = 2;
    const FID_AHASH: u64 = 3;
    const FID_HASH_DISTANCE: u64 = 4;
    const FID_VERSION: u64 = 5;

    struct Ext;

    // ---- Arg extraction ----
    //
    // For image inputs: BLOB or TEXT (raw bytes) is accepted; NULL or
    // anything else returns None. Per spec we never raise on bad image
    // bytes  unparseable returns NULL.
    fn opt_bytes(args: &[SqlValue], i: usize) -> Option<Vec<u8>> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    // For hash_distance: must be BLOBs (TEXT might be hex/base64; we
    // don't want to silently accept ambiguous encodings). NULL -> None.
    fn opt_blob_strict(args: &[SqlValue], i: usize) -> Option<Vec<u8>> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            _ => None,
        }
    }

    // ---- Image decode ----
    //
    // Decode an arbitrary image blob to a `(width, height, gray)`
    // triple where `gray` is row-major u8 luma. PNG and JPEG only;
    // anything else returns None. Decoder errors map to None (we
    // never raise to SQL on a bad image).
    fn decode_gray(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        if looks_like_png(bytes) {
            decode_png_gray(bytes)
        } else if looks_like_jpeg(bytes) {
            decode_jpeg_gray(bytes)
        } else {
            None
        }
    }

    fn looks_like_png(b: &[u8]) -> bool {
        b.len() >= 8 && &b[..8] == b"\x89PNG\r\n\x1a\n"
    }

    fn looks_like_jpeg(b: &[u8]) -> bool {
        // SOI = 0xFFD8, followed by another marker (0xFF...).
        b.len() >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF
    }

    fn decode_png_gray(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        let decoder = png::Decoder::new(bytes);
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).ok()?;
        let w = info.width;
        let h = info.height;
        let frame = &buf[..info.buffer_size()];
        let color = info.color_type;
        let bit_depth = info.bit_depth;
        // png 0.17 normalizes to 8 or 16 bit; we handle 8-bit pixels
        // for the common color types. 16-bit comes through as
        // big-endian pairs; we take the high byte for luma.
        let bpp_in = match bit_depth {
            png::BitDepth::Eight => 1,
            png::BitDepth::Sixteen => 2,
            // 1/2/4-bit indexed/grayscale exists but the next_frame
            // path normalizes those up to 8-bit before delivery in
            // practice; if a transformer isn't applied we punt.
            _ => return None,
        };
        let channels = match color {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            // Indexed should have been expanded by the transformer.
            // If it wasn't, give up rather than misinterpret.
            png::ColorType::Indexed => return None,
        };
        let stride = (w as usize) * channels * bpp_in;
        if frame.len() < stride * h as usize {
            return None;
        }
        let mut gray = Vec::with_capacity((w * h) as usize);
        for y in 0..h as usize {
            let row = &frame[y * stride..y * stride + stride];
            for x in 0..w as usize {
                let p = &row[x * channels * bpp_in..x * channels * bpp_in + channels * bpp_in];
                // Per-channel u8 extraction (high byte if 16-bit).
                let ch = |i: usize| -> u8 {
                    if bpp_in == 2 {
                        p[i * 2]
                    } else {
                        p[i]
                    }
                };
                let luma = match channels {
                    1 => ch(0) as u32,
                    2 => ch(0) as u32, // gray + alpha; ignore alpha
                    3 => luma_from_rgb(ch(0), ch(1), ch(2)),
                    4 => luma_from_rgb(ch(0), ch(1), ch(2)),
                    _ => return None,
                };
                gray.push(luma as u8);
            }
        }
        Some((w, h, gray))
    }

    fn decode_jpeg_gray(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        let mut d = jpeg_decoder::Decoder::new(bytes);
        let pixels = d.decode().ok()?;
        let info = d.info()?;
        let w = info.width as u32;
        let h = info.height as u32;
        let channels = match info.pixel_format {
            jpeg_decoder::PixelFormat::L8 => 1,
            jpeg_decoder::PixelFormat::RGB24 => 3,
            jpeg_decoder::PixelFormat::CMYK32 => 4,
            // L16 is rare for JPEG; not worth a separate path.
            _ => return None,
        };
        if pixels.len() < (w * h) as usize * channels {
            return None;
        }
        let mut gray = Vec::with_capacity((w * h) as usize);
        for i in 0..(w * h) as usize {
            let p = &pixels[i * channels..i * channels + channels];
            let luma = match channels {
                1 => p[0] as u32,
                3 => luma_from_rgb(p[0], p[1], p[2]),
                4 => {
                    // CMYK -> RGB approximation, then luma. Good enough
                    // for perceptual hashing where exact color fidelity
                    // doesn't matter.
                    let c = p[0] as u32;
                    let m = p[1] as u32;
                    let y = p[2] as u32;
                    let k = p[3] as u32;
                    let r = (255 * (255 - c) * (255 - k)) / (255 * 255);
                    let g = (255 * (255 - m) * (255 - k)) / (255 * 255);
                    let b = (255 * (255 - y) * (255 - k)) / (255 * 255);
                    luma_from_rgb(r as u8, g as u8, b as u8)
                }
                _ => return None,
            };
            gray.push(luma as u8);
        }
        Some((w, h, gray))
    }

    /// Rec. 601 luma weights, integer-scaled. Standard for hashing
    /// since the chroma sensitivity of the eye doesn't matter when
    /// you're going to threshold to a single bit anyway.
    fn luma_from_rgb(r: u8, g: u8, b: u8) -> u32 {
        (299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000
    }

    // ---- Resize (nearest neighbor) ----
    //
    // Perceptual hashes are notoriously insensitive to the resize
    // filter  bilinear is marginally nicer for natural images but
    // nearest-neighbor is what most reference implementations use for
    // small targets like 8x8 / 32x32, and it keeps the code small.
    // The dominant signal is "low-frequency luma layout"; the
    // downsampling kernel is in the noise.
    fn resize_nearest(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((dw * dh) as usize);
        for y in 0..dh {
            // +sh/(2*dh) bias  sample pixel centers, not edges.
            let sy = ((y as u64) * sh as u64 + (sh as u64) / 2) / dh as u64;
            let sy = sy.min(sh as u64 - 1) as u32;
            for x in 0..dw {
                let sx = ((x as u64) * sw as u64 + (sw as u64) / 2) / dw as u64;
                let sx = sx.min(sw as u64 - 1) as u32;
                out.push(src[(sy * sw + sx) as usize]);
            }
        }
        out
    }

    // ---- Hash assembly ----
    //
    // bits is a length-64 slice of 0/1; bit i lives at byte i/8, in MSB
    // position i%8. This matches img_hash and the ImageHash Python
    // library  important if anyone wants to round-trip hashes across
    // toolchains.
    fn bits_to_bytes(bits: &[u8; 64]) -> [u8; 8] {
        let mut out = [0u8; 8];
        for (i, &b) in bits.iter().enumerate() {
            if b != 0 {
                out[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out
    }

    // ---- aHash ----
    fn ahash(bytes: &[u8]) -> Option<[u8; 8]> {
        let (w, h, gray) = decode_gray(bytes)?;
        if w == 0 || h == 0 {
            return None;
        }
        let small = resize_nearest(&gray, w, h, 8, 8);
        let sum: u32 = small.iter().map(|&p| p as u32).sum();
        let mean = (sum / 64) as u8;
        let mut bits = [0u8; 64];
        for (i, &p) in small.iter().enumerate() {
            bits[i] = if p >= mean { 1 } else { 0 };
        }
        Some(bits_to_bytes(&bits))
    }

    // ---- dHash ----
    fn dhash(bytes: &[u8]) -> Option<[u8; 8]> {
        let (w, h, gray) = decode_gray(bytes)?;
        if w == 0 || h == 0 {
            return None;
        }
        // 9 wide so we get 8 horizontal differences per row.
        let small = resize_nearest(&gray, w, h, 9, 8);
        let mut bits = [0u8; 64];
        for row in 0..8usize {
            for col in 0..8usize {
                let left = small[row * 9 + col];
                let right = small[row * 9 + col + 1];
                bits[row * 8 + col] = if left > right { 1 } else { 0 };
            }
        }
        Some(bits_to_bytes(&bits))
    }

    // ---- pHash (DCT-II) ----
    //
    // Resize to 32x32, run the 2D DCT-II, keep the top-left 8x8, drop
    // the DC term, threshold against the median of the remaining 63
    // AC coefficients. The DC bit (position 0) is conventionally 0
    // (median == median).
    fn phash(bytes: &[u8]) -> Option<[u8; 8]> {
        const N: usize = 32;
        const K: usize = 8;
        let (w, h, gray) = decode_gray(bytes)?;
        if w == 0 || h == 0 {
            return None;
        }
        let small = resize_nearest(&gray, w, h, N as u32, N as u32);
        // Cast to f32 for DCT math.
        let mut mat = [[0f32; N]; N];
        for y in 0..N {
            for x in 0..N {
                mat[y][x] = small[y * N + x] as f32;
            }
        }
        // 2D DCT = DCT on rows then DCT on cols of the result.
        // The constant scale factors cancel out for thresholding so
        // we skip them  saves a multiply per element with no effect
        // on the bit pattern.
        let cos_table = build_cos_table::<N>();
        let mut row_dct = [[0f32; N]; N];
        for y in 0..N {
            for u in 0..N {
                let mut s = 0f32;
                for x in 0..N {
                    s += mat[y][x] * cos_table[u][x];
                }
                row_dct[y][u] = s;
            }
        }
        let mut col_dct = [[0f32; N]; N];
        for u in 0..N {
            for v in 0..N {
                let mut s = 0f32;
                for y in 0..N {
                    s += row_dct[y][u] * cos_table[v][y];
                }
                col_dct[v][u] = s;
            }
        }
        // Extract top-left KxK.
        let mut block = [0f32; K * K];
        for v in 0..K {
            for u in 0..K {
                block[v * K + u] = col_dct[v][u];
            }
        }
        // Median of AC coefficients (skip index 0 = DC).
        let mut ac = [0f32; K * K - 1];
        ac.copy_from_slice(&block[1..]);
        // Partial sort to find the median; only 63 entries so a
        // selection sort is fine and keeps wasm size down vs pulling
        // a sort lib.
        let median = median_of(&mut ac);
        let mut bits = [0u8; 64];
        // Convention: DC bit = 0. Then AC bits.
        bits[0] = 0;
        for i in 1..(K * K) {
            bits[i] = if block[i] > median { 1 } else { 0 };
        }
        Some(bits_to_bytes(&bits))
    }

    /// Pre-compute cos((2x+1) * u * PI / (2N)) for all (u, x).
    /// PI hard-coded; std::f32::consts::PI isn't available in our
    /// `extern crate alloc` setup but the value is fine to inline.
    fn build_cos_table<const N: usize>() -> [[f32; N]; N] {
        const PI: f32 = core::f32::consts::PI;
        let mut t = [[0f32; N]; N];
        for u in 0..N {
            for x in 0..N {
                let arg = ((2 * x + 1) as f32) * (u as f32) * PI / (2.0 * N as f32);
                t[u][x] = cosf(arg);
            }
        }
        t
    }

    /// f32 cosine via libm-free Taylor reduction. We don't have libm
    /// linked at the wit-bindgen level, and core::f32::cos isn't a
    /// const intrinsic at runtime here either  the cleanest fix is
    /// to call it through the standard method. f32::cos is available
    /// in core on wasm32-wasip2 via the compiler's intrinsics path.
    fn cosf(x: f32) -> f32 {
        // f32::cos exists in core for wasm targets (lowered to a
        // compiler intrinsic). If a future toolchain regression
        // removes it we'd need a Horner-form polynomial reduction
        // here; for now this is the simplest correct path.
        libm_cos(x)
    }

    /// Range-reduced Taylor cosine, accurate to ~1e-6 over the input
    /// range we hit (arguments stay within ~[0, 50] for N=32, but
    /// reduction brings them to [-PI, PI]).
    ///
    /// We roll our own to avoid any libm linkage surprises on
    /// wasm32-wasip2 (some versions of std on this target expose
    /// f32::cos only via a libm extern that the wit-bindgen reactor
    /// build doesn't pull in).
    fn libm_cos(x: f32) -> f32 {
        const PI: f32 = core::f32::consts::PI;
        const TWO_PI: f32 = 2.0 * PI;
        // Reduce mod 2*PI to [-PI, PI].
        let mut a = x % TWO_PI;
        if a > PI {
            a -= TWO_PI;
        } else if a < -PI {
            a += TWO_PI;
        }
        // cos(a) for a in [-PI, PI] via Taylor series, 8 terms.
        // Accuracy: |err| < 5e-7 across the band  more than enough
        // for a thresholded pHash.
        let a2 = a * a;
        let mut term = 1.0f32;
        let mut sum = 1.0f32;
        // term_k = -term_{k-1} * a^2 / ((2k-1)(2k))
        for k in 1..=8 {
            let denom = ((2 * k - 1) * (2 * k)) as f32;
            term *= -a2 / denom;
            sum += term;
        }
        sum
    }

    fn median_of(slice: &mut [f32]) -> f32 {
        // Insertion sort; n=63 so this is fine.
        let n = slice.len();
        for i in 1..n {
            let mut j = i;
            while j > 0 && slice[j - 1] > slice[j] {
                slice.swap(j - 1, j);
                j -= 1;
            }
        }
        if n % 2 == 1 {
            slice[n / 2]
        } else {
            (slice[n / 2 - 1] + slice[n / 2]) / 2.0
        }
    }

    // ---- Hamming distance ----
    fn hash_distance(a: &[u8], b: &[u8]) -> Option<i64> {
        if a.len() != b.len() {
            return None;
        }
        let mut d = 0i64;
        for i in 0..a.len() {
            d += (a[i] ^ b[i]).count_ones() as i64;
        }
        Some(d)
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "perceptual_hash".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: vec![
                    s(FID_PHASH, "phash", 1, det),
                    s(FID_DHASH, "dhash", 1, det),
                    s(FID_AHASH, "ahash", 1, det),
                    s(FID_HASH_DISTANCE, "hash_distance", 2, det),
                    s(FID_VERSION, "perceptual_hash_version", 0, det),
                ],
                aggregate_functions: vec![],
                collations: vec![],
                vtabs: vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: vec![],
                optional_capabilities: vec![],
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_PHASH => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    match phash(&bytes) {
                        Some(h) => Ok(SqlValue::Blob(h.to_vec())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_DHASH => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    match dhash(&bytes) {
                        Some(h) => Ok(SqlValue::Blob(h.to_vec())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_AHASH => {
                    let Some(bytes) = opt_bytes(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    match ahash(&bytes) {
                        Some(h) => Ok(SqlValue::Blob(h.to_vec())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_HASH_DISTANCE => {
                    let Some(a) = opt_blob_strict(&args, 0) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(b) = opt_blob_strict(&args, 1) else {
                        return Ok(SqlValue::Null);
                    };
                    match hash_distance(&a, &b) {
                        Some(d) => Ok(SqlValue::Integer(d)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("perceptual_hash: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
