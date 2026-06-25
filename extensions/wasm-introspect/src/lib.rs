//! Wasm module introspection scalars.
//!
//! Function surface:
//!
//!   wasm_is_valid(blob)         -> integer 0/1
//!   wasm_imports(blob)          -> text JSON [{module, name, kind}, ...]
//!   wasm_exports(blob)          -> text JSON [{name, kind}, ...]
//!   wasm_custom_sections(blob)  -> text JSON [name, ...]
//!   wasm_function_count(blob)   -> integer (declared + imported functions)
//!   wasm_memory_pages(blob)     -> integer (initial pages of first memory; 64KB each)
//!   wasm_version_byte(blob)     -> integer (byte 4 of header: 0x01 = MVP module,
//!                                           0x0d = component)
//!   wasm_introspect_version()   -> text
//!
//! All blob-consuming scalars return NULL on parse failure  matches
//! the "bad row shouldn't kill the SELECT" pattern used by the other
//! parse-only extensions in this tree (asn1, tls-cert, pdf-meta...).
//!
//! NULL in -> NULL out for the blob argument.
//!
//! Both core wasm modules (magic `\0asm\x01\x00\x00\x00`) and
//! component-model components (magic `\0asm\x0d\x00\x01\x00`) are
//! recognised. wasm_version_byte returns the actual byte at offset 4
//! so callers can branch.

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

    use serde_json::{json, Value};
    use wasmparser::{
        ComponentExternalKind, ComponentTypeRef, Encoding, ExternalKind, Parser, Payload, TypeRef,
        Validator,
    };

    const FID_IS_VALID: u64 = 1;
    const FID_IMPORTS: u64 = 2;
    const FID_EXPORTS: u64 = 3;
    const FID_CUSTOM_SECTIONS: u64 = 4;
    const FID_FUNCTION_COUNT: u64 = 5;
    const FID_MEMORY_PAGES: u64 = 6;
    const FID_VERSION_BYTE: u64 = 7;
    const FID_VERSION: u64 = 8;

    struct Ext;

    // ----- value plumbing -----

    fn blob_arg(args: &[SqlValue], idx: usize, fname: &str) -> Result<Option<Vec<u8>>, String> {
        match args.get(idx) {
            None | Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Blob(b)) => Ok(Some(b.clone())),
            // TEXT accepted so callers can feed hex-decoded strings or
            // readfile()-d paths without a manual CAST.
            Some(SqlValue::Text(s)) => Ok(Some(s.as_bytes().to_vec())),
            _ => Err(format!("{fname}: arg {} must be BLOB / TEXT / NULL", idx + 1)),
        }
    }

    // ----- header sniffing -----

    /// Wasm + component-model magic prefix `\0asm`.
    const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6d];

    /// Return Some(version_byte) when the blob has the wasm magic.
    /// The byte at offset 4 distinguishes formats:
    ///   0x01 = core wasm MVP module
    ///   0x0d = component-model component
    ///   other (rare): future or experimental encodings; surfaced as-is.
    fn version_byte(b: &[u8]) -> Option<u8> {
        if b.len() >= 8 && b[..4] == WASM_MAGIC {
            Some(b[4])
        } else {
            None
        }
    }

    // ----- import / export kind mapping -----

    fn type_ref_kind(t: TypeRef) -> &'static str {
        match t {
            TypeRef::Func(_) => "function",
            TypeRef::Table(_) => "table",
            TypeRef::Memory(_) => "memory",
            TypeRef::Global(_) => "global",
            TypeRef::Tag(_) => "tag",
        }
    }

    fn external_kind(k: ExternalKind) -> &'static str {
        match k {
            ExternalKind::Func => "function",
            ExternalKind::Table => "table",
            ExternalKind::Memory => "memory",
            ExternalKind::Global => "global",
            ExternalKind::Tag => "tag",
        }
    }

    fn component_type_ref_kind(t: ComponentTypeRef) -> &'static str {
        match t {
            ComponentTypeRef::Module(_) => "module",
            ComponentTypeRef::Func(_) => "function",
            ComponentTypeRef::Value(_) => "value",
            ComponentTypeRef::Type(_) => "type",
            ComponentTypeRef::Instance(_) => "instance",
            ComponentTypeRef::Component(_) => "component",
        }
    }

    fn component_external_kind(k: ComponentExternalKind) -> &'static str {
        match k {
            ComponentExternalKind::Module => "module",
            ComponentExternalKind::Func => "function",
            ComponentExternalKind::Value => "value",
            ComponentExternalKind::Type => "type",
            ComponentExternalKind::Instance => "instance",
            ComponentExternalKind::Component => "component",
        }
    }

    // ----- collected stats -----

    /// Everything Parser::parse_all yielded that we care about. The
    /// recursive variants (`ComponentSection`/`ModuleSection`) are
    /// traversed automatically by parse_all  it emits payloads from
    /// every nested module/component in source order, so we just
    /// accumulate. For nested components the resulting JSON lists
    /// merge top-level + nested imports; that matches "everything in
    /// the blob" which is what the introspection surface promises.
    #[derive(Default)]
    struct Stats {
        imports: Vec<Value>,
        exports: Vec<Value>,
        custom_sections: Vec<String>,
        // Functions = imported function count + declared (FunctionSection)
        // count. Matches the wat / wasm spec's "function index space".
        imported_functions: u64,
        declared_functions: u64,
        // First memory's initial size in 64KB pages. Components don't
        // have a top-level memory section; we walk into nested modules.
        first_memory_pages: Option<u64>,
    }

    /// Stream the blob through wasmparser, collecting introspection
    /// data. Returns Err on any parse failure  the caller maps that
    /// to NULL.
    fn collect(b: &[u8]) -> Result<Stats, String> {
        let mut s = Stats::default();
        for payload in Parser::new(0).parse_all(b) {
            let p = payload.map_err(|e| format!("wasm parse: {e}"))?;
            match p {
                Payload::Version { .. } => {
                    // No-op  the header is sniffed separately by
                    // version_byte() for the dedicated scalar.
                }
                Payload::ImportSection(reader) => {
                    for imp in reader {
                        let imp = imp.map_err(|e| format!("import: {e}"))?;
                        if matches!(imp.ty, TypeRef::Func(_)) {
                            s.imported_functions += 1;
                        }
                        s.imports.push(json!({
                            "module": imp.module,
                            "name":   imp.name,
                            "kind":   type_ref_kind(imp.ty),
                        }));
                    }
                }
                Payload::ExportSection(reader) => {
                    for exp in reader {
                        let exp = exp.map_err(|e| format!("export: {e}"))?;
                        s.exports.push(json!({
                            "name": exp.name,
                            "kind": external_kind(exp.kind),
                        }));
                    }
                }
                Payload::FunctionSection(reader) => {
                    // count() consumes the reader but only counts
                    // entries (no payload validation past length).
                    s.declared_functions += reader.count() as u64;
                }
                Payload::MemorySection(reader) => {
                    for m in reader {
                        let m = m.map_err(|e| format!("memory: {e}"))?;
                        if s.first_memory_pages.is_none() {
                            s.first_memory_pages = Some(m.initial);
                        }
                    }
                }
                Payload::CustomSection(c) => {
                    s.custom_sections.push(c.name().to_string());
                }
                // ----- component-model payloads -----
                Payload::ComponentImportSection(reader) => {
                    for imp in reader {
                        let imp = imp.map_err(|e| format!("component import: {e}"))?;
                        s.imports.push(json!({
                            // Component imports use a single "name"
                            // identifier (a kebab-case wasi name like
                            // "wasi:cli/stdout@0.2.0"); there's no
                            // separate module field. We surface the
                            // full name in `name` and leave `module`
                            // empty for shape parity with core wasm.
                            "module": "",
                            "name":   imp.name.0,
                            "kind":   component_type_ref_kind(imp.ty),
                        }));
                    }
                }
                Payload::ComponentExportSection(reader) => {
                    for exp in reader {
                        let exp = exp.map_err(|e| format!("component export: {e}"))?;
                        s.exports.push(json!({
                            "name": exp.name.0,
                            "kind": component_external_kind(exp.kind),
                        }));
                    }
                }
                // ModuleSection / ComponentSection / InstanceSection /
                // CoreTypeSection / TypeSection / etc.  parse_all
                // recurses into nested modules + components on its
                // own, so we don't have to manually re-enter; the
                // section *headers* aren't useful for introspection.
                _ => {}
            }
        }
        Ok(s)
    }

    // ----- impls -----

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
                name: "wasm_introspect".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: vec![
                    s(FID_IS_VALID, "wasm_is_valid", 1, det),
                    s(FID_IMPORTS, "wasm_imports", 1, det),
                    s(FID_EXPORTS, "wasm_exports", 1, det),
                    s(FID_CUSTOM_SECTIONS, "wasm_custom_sections", 1, det),
                    s(FID_FUNCTION_COUNT, "wasm_function_count", 1, det),
                    s(FID_MEMORY_PAGES, "wasm_memory_pages", 1, det),
                    s(FID_VERSION_BYTE, "wasm_version_byte", 1, det),
                    s(FID_VERSION, "wasm_introspect_version", 0, det),
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
                preferred_prefix: Some("wasm_introspect".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.wasm_introspect".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_IS_VALID => {
                    let b = match blob_arg(&args, 0, "wasm_is_valid")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Integer(0)),
                    };
                    // Full structural + type validation. Validator
                    // handles core modules and components; the
                    // Encoding it returns from Version is just
                    // informational. `validate_all` swallows the
                    // payloads, so for the other scalars we use a
                    // fresh Parser.
                    let mut v = Validator::new();
                    let ok = v.validate_all(&b).is_ok();
                    Ok(SqlValue::Integer(if ok { 1 } else { 0 }))
                }
                FID_IMPORTS => {
                    let b = match blob_arg(&args, 0, "wasm_imports")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    match collect(&b) {
                        Ok(s) => Ok(SqlValue::Text(
                            Value::Array(s.imports).to_string(),
                        )),
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_EXPORTS => {
                    let b = match blob_arg(&args, 0, "wasm_exports")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    match collect(&b) {
                        Ok(s) => Ok(SqlValue::Text(
                            Value::Array(s.exports).to_string(),
                        )),
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_CUSTOM_SECTIONS => {
                    let b = match blob_arg(&args, 0, "wasm_custom_sections")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    match collect(&b) {
                        Ok(s) => {
                            let arr: Vec<Value> =
                                s.custom_sections.into_iter().map(Value::String).collect();
                            Ok(SqlValue::Text(Value::Array(arr).to_string()))
                        }
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_FUNCTION_COUNT => {
                    let b = match blob_arg(&args, 0, "wasm_function_count")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    match collect(&b) {
                        Ok(s) => Ok(SqlValue::Integer(
                            (s.imported_functions + s.declared_functions) as i64,
                        )),
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_MEMORY_PAGES => {
                    let b = match blob_arg(&args, 0, "wasm_memory_pages")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    match collect(&b) {
                        Ok(s) => match s.first_memory_pages {
                            Some(p) => Ok(SqlValue::Integer(p as i64)),
                            // No memory section at all (e.g. a pure
                            // function-only module). Distinct from
                            // parse failure  return 0 rather than
                            // NULL.
                            None => Ok(SqlValue::Integer(0)),
                        },
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_VERSION_BYTE => {
                    let b = match blob_arg(&args, 0, "wasm_version_byte")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match version_byte(&b) {
                        Some(v) => SqlValue::Integer(v as i64),
                        None => SqlValue::Null,
                    })
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "wasm-introspect (wasmparser 0.215); extension {}",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("wasm_introspect: unknown func id {other}")),
            }
        }
    }

    // Silence unused-import warnings  Encoding is reserved for a
    // future "wasm_encoding(blob) -> text" if we ever need to split
    // 'module' vs 'component' as words.
    #[allow(dead_code)]
    fn _unused(_e: Encoding) {}

    bindings::export!(Ext with_types_in bindings);
}
