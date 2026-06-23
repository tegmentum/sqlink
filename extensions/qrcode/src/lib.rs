//! QR code generation for SQLite (ISO/IEC 18004).
//!
//! Natural pair with `totp.totp_url()` -- build an `otpauth://` URI
//! in SQL, then render it to a scannable SVG (or unicode terminal
//! block) without leaving the database.
//!
//! Function surface (PLAN-more-extensions-3.md #6):
//!
//!   qr_svg(text, [ecc])         -> text    (full SVG document)
//!   qr_unicode(text, [ecc])     -> text    (terminal-friendly, 2-rows-per-line)
//!   qr_modules(text, [ecc])     -> text    (JSON 2D 0/1 grid)
//!   qr_size(text, [ecc])        -> integer (modules per side after encoding)
//!   qr_version_for(text, [ecc]) -> integer (QR symbol version 1-40)
//!   qrcode_version()            -> text
//!
//! `ecc` is the optional error-correction level: 'L' (~7%),
//! 'M' (~15%, default), 'Q' (~25%), 'H' (~30%). NULL -> NULL.

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

    use qrcode::render::svg as svg_render;
    use qrcode::render::unicode::Dense1x2;
    use qrcode::{Color, EcLevel, QrCode, Version};

    // ---- Function IDs ----
    const FID_SVG: u64 = 1;
    const FID_UNICODE: u64 = 2;
    const FID_MODULES: u64 = 3;
    const FID_SIZE: u64 = 4;
    const FID_VERSION_FOR: u64 = 5;
    const FID_VERSION: u64 = 6;

    struct Ext;

    /// Parse the optional ECC level arg. Accepts 'L', 'M', 'Q', 'H'
    /// (case-insensitive). NULL/missing -> default 'M' (qrcode crate
    /// default). Anything else -> hard error so a typo doesn't get
    /// silently downgraded.
    fn parse_ecc(args: &[SqlValue], idx: usize, fname: &str) -> Result<EcLevel, String> {
        match args.get(idx) {
            None | Some(SqlValue::Null) => Ok(EcLevel::M),
            Some(SqlValue::Text(s)) => {
                let trimmed = s.trim();
                if trimmed.len() != 1 {
                    return Err(format!(
                        "{fname}: ecc must be one of L/M/Q/H (got {s:?})"
                    ));
                }
                match trimmed.chars().next().unwrap().to_ascii_uppercase() {
                    'L' => Ok(EcLevel::L),
                    'M' => Ok(EcLevel::M),
                    'Q' => Ok(EcLevel::Q),
                    'H' => Ok(EcLevel::H),
                    _ => Err(format!(
                        "{fname}: ecc must be one of L/M/Q/H (got {s:?})"
                    )),
                }
            }
            _ => Err(format!("{fname}: ecc must be TEXT (L/M/Q/H)")),
        }
    }

    /// Coerce SqlValue at index 0 to the bytes to encode. TEXT and
    /// BLOB pass through; INTEGER/REAL render as their TEXT form.
    /// NULL is the "propagate NULL" sentinel -- callers convert that
    /// into `Ok(None)`.
    fn payload_bytes(args: &[SqlValue], fname: &str) -> Result<Option<Vec<u8>>, String> {
        match args.first() {
            Some(SqlValue::Text(s)) => Ok(Some(s.as_bytes().to_vec())),
            Some(SqlValue::Blob(b)) => Ok(Some(b.clone())),
            Some(SqlValue::Integer(n)) => Ok(Some(n.to_string().into_bytes())),
            Some(SqlValue::Real(r)) => Ok(Some(r.to_string().into_bytes())),
            Some(SqlValue::Null) => Ok(None),
            None => Err(format!("{fname}: missing data arg")),
        }
    }

    /// Build a QrCode at the requested ECC. Surfaces qrcode crate
    /// errors as plain strings -- the caller is responsible for
    /// turning empty / NULL / too-large input into the right SQL
    /// sentinel.
    fn build(data: &[u8], ec: EcLevel) -> Result<QrCode, String> {
        QrCode::with_error_correction_level(data, ec)
            .map_err(|e| format!("qrcode: {e:?}"))
    }

    /// The integer 1..40 for `Version::Normal(n)`, mapped negative
    /// for `Version::Micro(n)` so callers can still distinguish.
    /// In practice with_error_correction_level only emits Normal
    /// versions, but we handle both for defensiveness.
    fn version_to_int(v: Version) -> i64 {
        match v {
            Version::Normal(n) => n as i64,
            Version::Micro(n) => -(n as i64),
        }
    }

    /// Render the QR code as a JSON 2D array of 0/1, row-major
    /// (y outer, x inner). No quiet-zone padding -- the caller can
    /// pad if they want. Keeps output compact: no whitespace, single
    /// digit per module.
    fn modules_to_json(code: &QrCode) -> String {
        let width = code.width();
        // Rough capacity: 2 brackets + width * (width * 2 + 3) bytes.
        let mut out = String::with_capacity(2 + width * (width * 2 + 3));
        out.push('[');
        for y in 0..width {
            if y > 0 {
                out.push(',');
            }
            out.push('[');
            for x in 0..width {
                if x > 0 {
                    out.push(',');
                }
                // Color::Dark = on (1), Color::Light = off (0).
                let on = code[(x, y)] != Color::Light;
                out.push(if on { '1' } else { '0' });
            }
            out.push(']');
        }
        out.push(']');
        out
    }

    /// Strip the `<?xml ?>` prolog so qr_svg returns a document
    /// that starts with `<svg` (the plan's acceptance spelling).
    /// The qrcode crate emits the prolog by default, which is fine
    /// for standalone files but trips the acceptance check and is
    /// noise when inlining into HTML.
    fn strip_xml_prolog(s: String) -> String {
        if let Some(rest) = s.strip_prefix("<?xml") {
            if let Some(idx) = rest.find("?>") {
                // Skip past `?>` (2 bytes) and any leading whitespace
                // before the `<svg`. In practice the crate emits no
                // whitespace between, but be defensive.
                let tail = &rest[idx + 2..];
                return tail.trim_start().to_string();
            }
        }
        s
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
                name: "qrcode".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // num_args = -1 advertises a variadic surface so
                    // the optional ecc arg is callable.
                    s(FID_SVG, "qr_svg", -1, det),
                    s(FID_UNICODE, "qr_unicode", -1, det),
                    s(FID_MODULES, "qr_modules", -1, det),
                    s(FID_SIZE, "qr_size", -1, det),
                    s(FID_VERSION_FOR, "qr_version_for", -1, det),
                    s(FID_VERSION, "qrcode_version", 0, det),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_SVG => {
                    let data = match payload_bytes(&args, "qr_svg")? {
                        Some(d) => d,
                        None => return Ok(SqlValue::Null),
                    };
                    let ec = parse_ecc(&args, 1, "qr_svg")?;
                    let code = build(&data, ec)?;
                    let svg = code.render::<svg_render::Color>().build();
                    Ok(SqlValue::Text(strip_xml_prolog(svg)))
                }
                FID_UNICODE => {
                    let data = match payload_bytes(&args, "qr_unicode")? {
                        Some(d) => d,
                        None => return Ok(SqlValue::Null),
                    };
                    let ec = parse_ecc(&args, 1, "qr_unicode")?;
                    let code = build(&data, ec)?;
                    let s = code
                        .render::<Dense1x2>()
                        .dark_color(Dense1x2::Dark)
                        .light_color(Dense1x2::Light)
                        .build();
                    Ok(SqlValue::Text(s))
                }
                FID_MODULES => {
                    let data = match payload_bytes(&args, "qr_modules")? {
                        Some(d) => d,
                        None => return Ok(SqlValue::Null),
                    };
                    let ec = parse_ecc(&args, 1, "qr_modules")?;
                    let code = build(&data, ec)?;
                    Ok(SqlValue::Text(modules_to_json(&code)))
                }
                FID_SIZE => {
                    let data = match payload_bytes(&args, "qr_size")? {
                        Some(d) => d,
                        None => return Ok(SqlValue::Null),
                    };
                    let ec = parse_ecc(&args, 1, "qr_size")?;
                    let code = build(&data, ec)?;
                    Ok(SqlValue::Integer(code.width() as i64))
                }
                FID_VERSION_FOR => {
                    let data = match payload_bytes(&args, "qr_version_for")? {
                        Some(d) => d,
                        None => return Ok(SqlValue::Null),
                    };
                    let ec = parse_ecc(&args, 1, "qr_version_for")?;
                    let code = build(&data, ec)?;
                    Ok(SqlValue::Integer(version_to_int(code.version())))
                }
                FID_VERSION => {
                    let v = format!(
                        "qrcode 0.14; extension {}",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }
                other => Err(format!("qrcode: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
