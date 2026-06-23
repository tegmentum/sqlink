//! Geo helpers: H3 + geohash + Maidenhead.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ── H3 ──────────────────────────────────────────────────────

use h3o::{CellIndex, LatLng, Resolution};

fn parse_resolution(r: i64) -> Result<Resolution, String> {
    Resolution::try_from(r as u8).map_err(|e| alloc::format!("h3: bad resolution {r}: {e}"))
}

pub fn h3_to_cell(lat: f64, lon: f64, res: i64) -> Result<String, String> {
    let resolution = parse_resolution(res)?;
    let latlng = LatLng::new(lat, lon).map_err(|e| alloc::format!("h3: bad coords: {e}"))?;
    Ok(latlng.to_cell(resolution).to_string())
}

fn parse_cell(s: &str) -> Result<CellIndex, String> {
    s.parse::<CellIndex>().map_err(|e| alloc::format!("h3: bad cell {s:?}: {e}"))
}

pub fn h3_to_geo(cell: &str) -> Result<String, String> {
    let c = parse_cell(cell)?;
    let l = LatLng::from(c);
    Ok(alloc::format!("{},{}", l.lat(), l.lng()))
}

pub fn h3_resolution(cell: &str) -> Result<i64, String> {
    Ok(parse_cell(cell)?.resolution() as i64)
}

pub fn h3_neighbors(cell: &str) -> Result<String, String> {
    let c = parse_cell(cell)?;
    let names: Vec<String> = c
        .grid_disk::<Vec<_>>(1)
        .into_iter()
        .filter(|n| *n != c)
        .map(|n| n.to_string())
        .collect();
    Ok(serde_json::Value::Array(
        names.into_iter().map(serde_json::Value::String).collect(),
    )
    .to_string())
}

pub fn h3_is_pentagon(cell: &str) -> Result<bool, String> {
    Ok(parse_cell(cell)?.is_pentagon())
}

// ── Geohash ─────────────────────────────────────────────────
//
// Hand-rolled. Standard 32-char base32 alphabet:
//   0123456789bcdefghjkmnpqrstuvwxyz
// Encodes by interleaving longitude / latitude bits and
// translating 5-bit chunks. Implementation follows
// Wikipedia's reference pseudocode.

const GEOHASH_ALPHABET: &[u8; 32] = b"0123456789bcdefghjkmnpqrstuvwxyz";

pub fn geohash_encode(lat: f64, lon: f64, precision: usize) -> Result<String, String> {
    if !(-90.0..=90.0).contains(&lat) {
        return Err(alloc::format!("geohash: lat {lat} out of range"));
    }
    if !(-180.0..=180.0).contains(&lon) {
        return Err(alloc::format!("geohash: lon {lon} out of range"));
    }
    let precision = precision.clamp(1, 22);
    let mut lat_range = (-90.0_f64, 90.0_f64);
    let mut lon_range = (-180.0_f64, 180.0_f64);
    let mut out = String::with_capacity(precision);
    let mut bit = 0;
    let mut ch: u8 = 0;
    let mut is_lon = true;
    while out.len() < precision {
        let (mid, range) = if is_lon {
            ((lon_range.0 + lon_range.1) / 2.0, &mut lon_range)
        } else {
            ((lat_range.0 + lat_range.1) / 2.0, &mut lat_range)
        };
        let v = if is_lon { lon } else { lat };
        if v >= mid {
            ch = (ch << 1) | 1;
            range.0 = mid;
        } else {
            ch <<= 1;
            range.1 = mid;
        }
        is_lon = !is_lon;
        bit += 1;
        if bit == 5 {
            out.push(GEOHASH_ALPHABET[ch as usize] as char);
            bit = 0;
            ch = 0;
        }
    }
    Ok(out)
}

pub fn geohash_decode(hash: &str) -> Result<String, String> {
    let bbox = geohash_bbox_raw(hash)?;
    let (s, w, n, e) = bbox;
    Ok(alloc::format!("{},{}", (s + n) / 2.0, (w + e) / 2.0))
}

fn geohash_bbox_raw(hash: &str) -> Result<(f64, f64, f64, f64), String> {
    let mut lat = (-90.0_f64, 90.0_f64);
    let mut lon = (-180.0_f64, 180.0_f64);
    let mut is_lon = true;
    for ch in hash.chars() {
        let idx = GEOHASH_ALPHABET
            .iter()
            .position(|&c| c as char == ch)
            .ok_or_else(|| alloc::format!("geohash: invalid char {ch:?}"))?;
        for bit in (0..5).rev() {
            let on = (idx >> bit) & 1 == 1;
            let range = if is_lon { &mut lon } else { &mut lat };
            let mid = (range.0 + range.1) / 2.0;
            if on {
                range.0 = mid;
            } else {
                range.1 = mid;
            }
            is_lon = !is_lon;
        }
    }
    Ok((lat.0, lon.0, lat.1, lon.1))
}

pub fn geohash_bbox(hash: &str) -> Result<String, String> {
    let (s, w, n, e) = geohash_bbox_raw(hash)?;
    Ok(serde_json::Value::Array(alloc::vec![
        serde_json::json!(s),
        serde_json::json!(w),
        serde_json::json!(n),
        serde_json::json!(e),
    ])
    .to_string())
}

pub fn geohash_neighbors(hash: &str) -> Result<String, String> {
    // 8-neighbour ring via incremental shifts. Cheaper to
    // recompute by encoding-with-a-nudge than to do the
    // "increment one base32 digit" trick correctly across
    // edge cases.
    let (s, w, n, e) = geohash_bbox_raw(hash)?;
    let lat_step = n - s;
    let lon_step = e - w;
    let center_lat = (s + n) / 2.0;
    let center_lon = (w + e) / 2.0;
    let prec = hash.len();
    let mut neighbors: Vec<String> = Vec::with_capacity(8);
    let offsets = [
        (-1.0, -1.0),
        (-1.0, 0.0),
        (-1.0, 1.0),
        (0.0, -1.0),
        (0.0, 1.0),
        (1.0, -1.0),
        (1.0, 0.0),
        (1.0, 1.0),
    ];
    for (dlat, dlon) in offsets {
        let nl = (center_lat + dlat * lat_step).clamp(-89.999, 89.999);
        let no = (center_lon + dlon * lon_step).clamp(-179.999, 179.999);
        if let Ok(h) = geohash_encode(nl, no, prec) {
            if h != hash {
                neighbors.push(h);
            }
        }
    }
    neighbors.sort();
    neighbors.dedup();
    Ok(serde_json::Value::Array(
        neighbors.into_iter().map(serde_json::Value::String).collect(),
    )
    .to_string())
}

// ── Maidenhead ──────────────────────────────────────────────
//
// 4-, 6-, or 8-character grid squares. Encoding:
//   field   2 chars  AR    20-deg lon / 10-deg lat
//   square  2 chars  09    2-deg lon / 1-deg lat
//   subsq   2 chars  ax    5-min lon / 2.5-min lat
//   extsq   2 chars  09    finer subdivision

pub fn maidenhead_encode(lat: f64, lon: f64, precision: usize) -> Result<String, String> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err("maidenhead: coords out of range".to_string());
    }
    let precision = precision.clamp(1, 4); // 1-4 pairs = 2-8 chars
    let lon = lon + 180.0;
    let lat = lat + 90.0;
    let mut out = String::with_capacity(precision * 2);

    // Pair 1: field (letters A-R)
    let field_lon = (lon / 20.0) as usize;
    let field_lat = (lat / 10.0) as usize;
    out.push((b'A' + field_lon as u8) as char);
    out.push((b'A' + field_lat as u8) as char);
    if precision == 1 {
        return Ok(out);
    }
    let lon = lon - (field_lon as f64) * 20.0;
    let lat = lat - (field_lat as f64) * 10.0;

    // Pair 2: square (digits 0-9)
    let sq_lon = (lon / 2.0) as usize;
    let sq_lat = lat as usize;
    out.push((b'0' + sq_lon as u8) as char);
    out.push((b'0' + sq_lat as u8) as char);
    if precision == 2 {
        return Ok(out);
    }
    let lon = lon - (sq_lon as f64) * 2.0;
    let lat = lat - sq_lat as f64;

    // Pair 3: subsquare (letters a-x)  2 deg lon / 24 chars
    // = 5-min lon per char.
    let sub_lon = ((lon / 2.0) * 24.0) as usize;
    let sub_lat = (lat * 24.0) as usize;
    out.push((b'a' + sub_lon as u8) as char);
    out.push((b'a' + sub_lat as u8) as char);
    if precision == 3 {
        return Ok(out);
    }
    // Pair 4: extended (digits)
    let lon = (lon / 2.0) * 24.0 - (sub_lon as f64);
    let lat = lat * 24.0 - sub_lat as f64;
    let ext_lon = (lon * 10.0) as usize;
    let ext_lat = (lat * 10.0) as usize;
    out.push((b'0' + ext_lon as u8) as char);
    out.push((b'0' + ext_lat as u8) as char);
    Ok(out)
}

pub fn maidenhead_decode(grid: &str) -> Result<String, String> {
    if grid.len() < 2 || grid.len() % 2 != 0 {
        return Err("maidenhead: grid length must be 2, 4, 6, or 8".to_string());
    }
    let bytes = grid.as_bytes();
    let f_lon = (bytes[0].to_ascii_uppercase() - b'A') as f64;
    let f_lat = (bytes[1].to_ascii_uppercase() - b'A') as f64;
    let mut lon = f_lon * 20.0 - 180.0;
    let mut lat = f_lat * 10.0 - 90.0;
    let mut lon_size = 20.0;
    let mut lat_size = 10.0;
    if grid.len() >= 4 {
        let s_lon = (bytes[2] - b'0') as f64;
        let s_lat = (bytes[3] - b'0') as f64;
        lon += s_lon * 2.0;
        lat += s_lat;
        lon_size = 2.0;
        lat_size = 1.0;
    }
    if grid.len() >= 6 {
        let sub_lon = (bytes[4].to_ascii_lowercase() - b'a') as f64;
        let sub_lat = (bytes[5].to_ascii_lowercase() - b'a') as f64;
        lon += sub_lon * (2.0 / 24.0);
        lat += sub_lat * (1.0 / 24.0);
        lon_size = 2.0 / 24.0;
        lat_size = 1.0 / 24.0;
    }
    if grid.len() >= 8 {
        let e_lon = (bytes[6] - b'0') as f64;
        let e_lat = (bytes[7] - b'0') as f64;
        lon += e_lon * (lon_size / 10.0);
        lat += e_lat * (lat_size / 10.0);
        lon_size /= 10.0;
        lat_size /= 10.0;
    }
    // Center of the grid cell.
    Ok(alloc::format!("{},{}", lat + lat_size / 2.0, lon + lon_size / 2.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h3_cell_round_trip() {
        let cell = h3_to_cell(51.5074, -0.1278, 9).unwrap();
        let geo = h3_to_geo(&cell).unwrap();
        let parts: Vec<&str> = geo.split(',').collect();
        let lat: f64 = parts[0].parse().unwrap();
        let lon: f64 = parts[1].parse().unwrap();
        // res-9 cells span ~150m so the centroid is within
        // 0.01 deg of any input inside the cell.
        assert!((lat - 51.5074).abs() < 0.01);
        assert!((lon - (-0.1278)).abs() < 0.01);
        assert_eq!(h3_resolution(&cell).unwrap(), 9);
    }

    #[test]
    fn geohash_round_trip() {
        let h = geohash_encode(40.7128, -74.0060, 8).unwrap();
        // Manhattan should encode to "dr5regw3" or similar at p=8.
        assert!(h.starts_with("dr5r"));
        let g = geohash_decode(&h).unwrap();
        let parts: Vec<&str> = g.split(',').collect();
        let lat: f64 = parts[0].parse().unwrap();
        let lon: f64 = parts[1].parse().unwrap();
        assert!((lat - 40.7128).abs() < 0.001);
        assert!((lon - (-74.0060)).abs() < 0.001);
    }

    #[test]
    fn maidenhead_known_grid() {
        // Greenwich (51.5, -0.16) sits in IO91vl per ham
        // convention.
        let g = maidenhead_encode(51.5, -0.16, 3).unwrap();
        assert!(g.starts_with("IO91"), "{g}");
        let d = maidenhead_decode("IO91vl").unwrap();
        let parts: Vec<&str> = d.split(',').collect();
        let lat: f64 = parts[0].parse().unwrap();
        let lon: f64 = parts[1].parse().unwrap();
        assert!((lat - 51.5).abs() < 0.05);
        assert!((lon - (-0.16)).abs() < 0.1);
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

    const FID_H3_CELL: u64 = 1;
    const FID_H3_GEO: u64 = 2;
    const FID_H3_RES: u64 = 3;
    const FID_H3_NEIGH: u64 = 4;
    const FID_H3_PENT: u64 = 5;
    const FID_GH_ENC: u64 = 10;
    const FID_GH_DEC: u64 = 11;
    const FID_GH_BBOX: u64 = 12;
    const FID_GH_NEIGH: u64 = 13;
    const FID_MH_ENC: u64 = 20;
    const FID_MH_DEC: u64 = 21;

    struct Ext;

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
                name: "geo".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_H3_CELL, "h3_to_cell", 3),
                    s(FID_H3_GEO, "h3_to_geo", 1),
                    s(FID_H3_RES, "h3_resolution", 1),
                    s(FID_H3_NEIGH, "h3_neighbors", 1),
                    s(FID_H3_PENT, "h3_is_pentagon", 1),
                    s(FID_GH_ENC, "geohash_encode", 3),
                    s(FID_GH_DEC, "geohash_decode", 1),
                    s(FID_GH_BBOX, "geohash_bbox", 1),
                    s(FID_GH_NEIGH, "geohash_neighbors", 1),
                    s(FID_MH_ENC, "maidenhead_encode", 3),
                    s(FID_MH_DEC, "maidenhead_decode", 1),
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
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(r)) => Ok(*r as i64),
            _ => Err(format!("{fname}: integer arg at {i}")),
        }
    }
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_H3_CELL => {
                    let lat = arg_real(&args, 0, "h3_to_cell")?;
                    let lon = arg_real(&args, 1, "h3_to_cell")?;
                    let res = arg_int(&args, 2, "h3_to_cell")?;
                    super::h3_to_cell(lat, lon, res).map(SqlValue::Text)
                }
                FID_H3_GEO => super::h3_to_geo(&arg_text(&args, 0, "h3_to_geo")?)
                    .map(SqlValue::Text),
                FID_H3_RES => super::h3_resolution(&arg_text(&args, 0, "h3_resolution")?)
                    .map(SqlValue::Integer),
                FID_H3_NEIGH => super::h3_neighbors(&arg_text(&args, 0, "h3_neighbors")?)
                    .map(SqlValue::Text),
                FID_H3_PENT => super::h3_is_pentagon(&arg_text(&args, 0, "h3_is_pentagon")?)
                    .map(|b| SqlValue::Integer(b as i64)),
                FID_GH_ENC => {
                    let lat = arg_real(&args, 0, "geohash_encode")?;
                    let lon = arg_real(&args, 1, "geohash_encode")?;
                    let p = arg_int(&args, 2, "geohash_encode")? as usize;
                    super::geohash_encode(lat, lon, p).map(SqlValue::Text)
                }
                FID_GH_DEC => super::geohash_decode(&arg_text(&args, 0, "geohash_decode")?)
                    .map(SqlValue::Text),
                FID_GH_BBOX => super::geohash_bbox(&arg_text(&args, 0, "geohash_bbox")?)
                    .map(SqlValue::Text),
                FID_GH_NEIGH => super::geohash_neighbors(&arg_text(&args, 0, "geohash_neighbors")?)
                    .map(SqlValue::Text),
                FID_MH_ENC => {
                    let lat = arg_real(&args, 0, "maidenhead_encode")?;
                    let lon = arg_real(&args, 1, "maidenhead_encode")?;
                    let p = arg_int(&args, 2, "maidenhead_encode")? as usize;
                    super::maidenhead_encode(lat, lon, p).map(SqlValue::Text)
                }
                FID_MH_DEC => super::maidenhead_decode(&arg_text(&args, 0, "maidenhead_decode")?)
                    .map(SqlValue::Text),
                other => Err(format!("geo: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
