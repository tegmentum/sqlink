//! Web Mercator tile coords (xyz, quadkey)

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

    const FID_X: u64 = 1;
    const FID_Y: u64 = 2;
    const FID_LON: u64 = 3;
    const FID_LAT: u64 = 4;
    const FID_QUADKEY: u64 = 5;
    const FID_FROM_QUADKEY: u64 = 6;
    const FID_BBOX: u64 = 7;

    struct Ext;

    /// Web Mercator (EPSG:3857) is the de facto tile projection. Slippy
    /// map tile scheme (xyz): x in [0, 2^z), y in [0, 2^z), origin at
    /// the top-left (north-west) corner. Reference:
    /// https://wiki.openstreetmap.org/wiki/Slippy_map_tilenames

    fn tile_x(lon: f64, z: u32) -> u32 {
        let n = (1u64 << z) as f64;
        let x = ((lon + 180.0) / 360.0 * n).floor();
        x.clamp(0.0, n - 1.0) as u32
    }

    fn tile_y(lat: f64, z: u32) -> u32 {
        let n = (1u64 << z) as f64;
        // Mercator latitude clamped to [-85.05113, 85.05113] (the
        // standard web-mercator usable range  beyond, the y
        // coordinate blows up).
        let lat_rad = lat.clamp(-85.05112878, 85.05112878).to_radians();
        let y = ((1.0 - lat_rad.tan().asinh() / core::f64::consts::PI) / 2.0 * n).floor();
        y.clamp(0.0, n - 1.0) as u32
    }

    fn tile_lon(x: u32, z: u32) -> f64 {
        let n = (1u64 << z) as f64;
        x as f64 / n * 360.0 - 180.0
    }

    fn tile_lat(y: u32, z: u32) -> f64 {
        let n = (1u64 << z) as f64;
        let lat_rad = (core::f64::consts::PI * (1.0 - 2.0 * y as f64 / n)).sinh().atan();
        lat_rad.to_degrees()
    }

    /// Bing quadkey: interleave x and y bits as base-4 digits from
    /// MSB. Reference: https://learn.microsoft.com/en-us/bingmaps/articles/bing-maps-tile-system
    fn quadkey(x: u32, y: u32, z: u32) -> String {
        let mut out = String::with_capacity(z as usize);
        for i in (1..=z).rev() {
            let mask: u32 = 1 << (i - 1);
            let mut d: u8 = b'0';
            if (x & mask) != 0 { d += 1; }
            if (y & mask) != 0 { d += 2; }
            out.push(d as char);
        }
        out
    }

    /// Reverse of quadkey  recover (x, y, z) from the digit string.
    fn from_quadkey(q: &str) -> Option<(u32, u32, u32)> {
        let z = q.len() as u32;
        if z == 0 || z > 31 { return None; }
        let mut x: u32 = 0;
        let mut y: u32 = 0;
        for (i, c) in q.chars().enumerate() {
            let mask: u32 = 1 << (z as usize - 1 - i);
            match c {
                '0' => {}
                '1' => x |= mask,
                '2' => y |= mask,
                '3' => { x |= mask; y |= mask; }
                _ => return None,
            }
        }
        Some((x, y, z))
    }

    /// JSON {west, south, east, north} of the tile (x, y, z).
    fn bbox_json(x: u32, y: u32, z: u32) -> String {
        let w = tile_lon(x, z);
        let e = tile_lon(x + 1, z);
        let n = tile_lat(y, z);
        let s = tile_lat(y + 1, z);
        format!("{{\"west\":{w},\"south\":{s},\"east\":{e},\"north\":{n}}}")
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
                name: "tile".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_X, "tile_x", 2, det),
                    s(FID_Y, "tile_y", 2, det),
                    s(FID_LON, "tile_lon", 2, det),
                    s(FID_LAT, "tile_lat", 2, det),
                    s(FID_QUADKEY, "tile_quadkey", 3, det),
                    s(FID_FROM_QUADKEY, "tile_from_quadkey", 1, det),
                    s(FID_BBOX, "tile_bbox", 3, det),
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

    fn arg_real(args: &[SqlValue], i: usize, fname: &str) -> Result<f64, String> {
        match args.get(i) {
            Some(SqlValue::Real(r)) => Ok(*r),
            Some(SqlValue::Integer(n)) => Ok(*n as f64),
            _ => Err(format!("{fname}: numeric arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_X => {
                    let lon = arg_real(&args, 0, "tile_x")?;
                    let z = arg_int(&args, 1, "tile_x")? as u32;
                    Ok(SqlValue::Integer(tile_x(lon, z) as i64))
                }
                FID_Y => {
                    let lat = arg_real(&args, 0, "tile_y")?;
                    let z = arg_int(&args, 1, "tile_y")? as u32;
                    Ok(SqlValue::Integer(tile_y(lat, z) as i64))
                }
                FID_LON => {
                    let x = arg_int(&args, 0, "tile_lon")? as u32;
                    let z = arg_int(&args, 1, "tile_lon")? as u32;
                    Ok(SqlValue::Real(tile_lon(x, z)))
                }
                FID_LAT => {
                    let y = arg_int(&args, 0, "tile_lat")? as u32;
                    let z = arg_int(&args, 1, "tile_lat")? as u32;
                    Ok(SqlValue::Real(tile_lat(y, z)))
                }
                FID_QUADKEY => {
                    let x = arg_int(&args, 0, "tile_quadkey")? as u32;
                    let y = arg_int(&args, 1, "tile_quadkey")? as u32;
                    let z = arg_int(&args, 2, "tile_quadkey")? as u32;
                    Ok(SqlValue::Text(quadkey(x, y, z)))
                }
                FID_FROM_QUADKEY => {
                    let q = arg_text(&args, 0, "tile_from_quadkey")?;
                    Ok(from_quadkey(&q)
                        .map(|(x, y, z)| {
                            SqlValue::Text(format!("{{\"x\":{x},\"y\":{y},\"z\":{z}}}"))
                        })
                        .unwrap_or(SqlValue::Null))
                }
                FID_BBOX => {
                    let x = arg_int(&args, 0, "tile_bbox")? as u32;
                    let y = arg_int(&args, 1, "tile_bbox")? as u32;
                    let z = arg_int(&args, 2, "tile_bbox")? as u32;
                    Ok(SqlValue::Text(bbox_json(x, y, z)))
                }
                other => Err(format!("tile: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
