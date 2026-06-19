//! PMTiles v3 vtab + a metadata scalar.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;

    use oxigdal_pmtiles::pmtiles::{PmTilesReader, TileInfo};

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "tabular",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan,
    };
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const VTAB_ID: u64 = 1;
    const FID_METADATA: u64 = 1;

    #[derive(Clone)]
    struct Instance {
        path: String,
    }

    struct Cursor_ {
        instance_id: u64,
        tiles: Vec<TileInfo>,
        data: Vec<u8>,
        tile_data_offset: u64,
        idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> = RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor_>> = RefCell::new(HashMap::new());
    }

    struct Ext;

    fn parse_args(args: &[String]) -> Result<String, String> {
        if args.is_empty() {
            return Err("pmtiles: path required".into());
        }
        let first = args[0].trim();
        let path = if let Some((k, v)) = first.split_once('=') {
            if k.trim() != "path" {
                return Err(format!("pmtiles: unknown arg {k:?}"));
            }
            strip_quotes(v.trim()).to_string()
        } else {
            strip_quotes(first).to_string()
        };
        Ok(path)
    }

    fn strip_quotes(s: &str) -> &str {
        let s = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(s);
        s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
    }

    fn build_schema_sql() -> String {
        "CREATE TABLE x(tile_id INTEGER, z INTEGER, x INTEGER, y INTEGER, data BLOB)".to_string()
    }

    fn load(path: &str) -> Result<(Vec<TileInfo>, Vec<u8>, u64), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("pmtiles: open {path}: {e}"))?;
        let reader = PmTilesReader::from_bytes(bytes)
            .map_err(|e| format!("pmtiles: parse header: {e}"))?;
        let tile_data_offset = reader.header.tile_data_offset;
        let tiles = reader
            .enumerate_tiles()
            .map_err(|e| format!("pmtiles: enumerate: {e}"))?;
        // Reader owns the raw bytes  re-read for direct slice access.
        // (PmTilesReader doesn't expose its data field publicly; we
        // re-read since std::fs::read is cheap on the same kernel
        // page cache.)
        let data = std::fs::read(path).map_err(|e| format!("pmtiles: reread {path}: {e}"))?;
        Ok((tiles, data, tile_data_offset))
    }

    fn tile_bytes(data: &[u8], tile_data_offset: u64, t: &TileInfo) -> Vec<u8> {
        let start = (tile_data_offset + t.data_offset) as usize;
        let end = start.saturating_add(t.data_length as usize);
        if end > data.len() {
            return Vec::new();
        }
        data[start..end].to_vec()
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            Manifest {
                name: "pmtiles".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![ScalarFunctionSpec {
                    id: FID_METADATA,
                    name: "pmtiles_metadata".to_string(),
                    num_args: 1,
                    func_flags: det,
                }],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "pmtiles".to_string(),
                    eponymous: false,
                    mutable: false,
                }],
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
                FID_METADATA => {
                    let path = match args.first() {
                        Some(SqlValue::Text(s)) => s.clone(),
                        _ => return Err("pmtiles_metadata: TEXT path required".into()),
                    };
                    let bytes = std::fs::read(&path)
                        .map_err(|e| format!("pmtiles_metadata: open {path}: {e}"))?;
                    let reader = PmTilesReader::from_bytes(bytes)
                        .map_err(|e| format!("pmtiles_metadata: parse: {e}"))?;
                    let meta = reader
                        .metadata()
                        .map_err(|e| format!("pmtiles_metadata: read: {e}"))?;
                    let json = meta
                        .to_json()
                        .map_err(|e| format!("pmtiles_metadata: to_json: {e}"))?;
                    Ok(SqlValue::Text(json))
                }
                other => Err(format!("pmtiles: unknown func id {other}")),
            }
        }
    }

    impl VtabGuest for Ext {
        fn create(
            _: u64,
            instance_id: u64,
            _: String,
            _: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            let path = parse_args(&args)?;
            // Validate by trying to parse the header.
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("pmtiles: open {path}: {e}"))?;
            let _ = PmTilesReader::from_bytes(bytes)
                .map_err(|e| format!("pmtiles: parse header: {e}"))?;
            INSTANCES.with(|m| m.borrow_mut().insert(instance_id, Instance { path }));
            Ok(build_schema_sql())
        }
        fn connect(
            v: u64,
            id: u64,
            d: String,
            t: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            <Self as VtabGuest>::create(v, id, d, t, args)
        }
        fn destroy(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }
        fn disconnect(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }
        fn best_index(_: u64, _: u64, info: IndexInfo) -> Result<IndexPlan, String> {
            Ok(IndexPlan {
                constraint_usage: info
                    .constraints
                    .iter()
                    .map(|_| ConstraintUsage { argv_index: 0, omit: false })
                    .collect(),
                idx_num: 0,
                idx_str: None,
                estimated_cost: 1_000_000.0,
                estimated_rows: 1_000_000,
                orderby_consumed: false,
            })
        }
        fn open(_: u64, instance_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor_ {
                        instance_id,
                        tiles: Vec::new(),
                        data: Vec::new(),
                        tile_data_offset: 0,
                        idx: 0,
                    },
                )
            });
            Ok(())
        }
        fn close(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| m.borrow_mut().remove(&cursor_id));
            Ok(())
        }
        fn filter(
            _: u64,
            cursor_id: u64,
            _: i32,
            _: Option<String>,
            _: Vec<SqlValue>,
        ) -> Result<(), String> {
            let inst_id = CURSORS
                .with(|cm| cm.borrow().get(&cursor_id).map(|c| c.instance_id).unwrap_or(0));
            let inst = INSTANCES
                .with(|m| m.borrow().get(&inst_id).cloned())
                .ok_or_else(|| "pmtiles: instance not connected".to_string())?;
            let (tiles, data, tile_data_offset) = load(&inst.path)?;
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.tiles = tiles;
                    c.data = data;
                    c.tile_data_offset = tile_data_offset;
                    c.idx = 0;
                }
            });
            Ok(())
        }
        fn next(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.idx += 1;
                }
            });
            Ok(())
        }
        fn eof(_: u64, cursor_id: u64) -> bool {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| c.idx >= c.tiles.len())
                    .unwrap_or(true)
            })
        }
        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "pmtiles: cursor not open".to_string())?;
                let t = c.tiles.get(c.idx).ok_or_else(|| "pmtiles: row past EOF".to_string())?;
                Ok(match col {
                    0 => SqlValue::Integer(t.tile_id as i64),
                    1 => SqlValue::Integer(t.z as i64),
                    2 => SqlValue::Integer(t.x as i64),
                    3 => SqlValue::Integer(t.y as i64),
                    4 => SqlValue::Blob(tile_bytes(&c.data, c.tile_data_offset, t)),
                    other => return Err(format!("pmtiles: bad column index {other}")),
                })
            })
        }
        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .and_then(|c| c.tiles.get(c.idx).map(|t| t.tile_id as i64))
                    .ok_or_else(|| "pmtiles: cursor past EOF".to_string())
            })
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
