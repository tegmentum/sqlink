//! PDF document metadata extraction from blobs via `lopdf` 0.34
//! (pure-rust, no rendering). The document-side counterpart to
//! `exif` / `image-meta` — where those answer "what camera / what
//! dimensions", this answers "what title / what author / how many
//! pages / which PDF spec version".
//!
//! Function surface (PLAN-more-extensions-4.md §1):
//!
//!   pdf_title(blob)         -> TEXT
//!   pdf_author(blob)        -> TEXT
//!   pdf_subject(blob)       -> TEXT
//!   pdf_creator(blob)       -> TEXT
//!   pdf_producer(blob)      -> TEXT
//!   pdf_creation_date(blob) -> TEXT  (ISO 8601 when parseable)
//!   pdf_mod_date(blob)      -> TEXT  (ISO 8601 when parseable)
//!   pdf_page_count(blob)    -> INTEGER
//!   pdf_pdf_version(blob)   -> TEXT  (e.g. "1.7" / "2.0")
//!   pdf_is_encrypted(blob)  -> INTEGER (0 / 1)
//!   pdf_keywords(blob)      -> TEXT  (raw /Info /Keywords value)
//!   pdf_all(blob)           -> TEXT  (JSON object of every field)
//!   pdf_meta_version()      -> TEXT
//!
//! NULL contract: every accessor returns SQL NULL on
//!   - SqlValue::Null input
//!   - non-BLOB / non-TEXT input
//!   - blobs that don't parse as PDF at all
//!   - the requested /Info field being absent
//!
//! Special-case fallbacks beyond strict /Info dictionary lookup:
//!
//!   - `pdf_pdf_version` falls back to a raw header scan (`%PDF-x.y`
//!     in the first 1KB) when full parse fails. The PDF spec puts
//!     the version in byte 0..8 so even a truncated file with just
//!     the header line gives us the version — this matches the
//!     acceptance bullet "truncated PDF (header only) → version
//!     still extracts".
//!
//!   - `pdf_is_encrypted` walks the trailer dictionary directly
//!     (the `/Encrypt` entry's presence is the canonical signal)
//!     so an encrypted PDF still returns 1 even if downstream
//!     decryption isn't attempted.
//!
//! Errors are NEVER surfaced to SQL — every scalar collapses to NULL
//! on bad input, mirroring the established convention in `exif` /
//! `image-meta`. Each call re-parses the blob fresh; no shared state.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::fmt::Write as _;

    use lopdf::{Document, Object};

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

    // ---- Stable function IDs. Changing breaks the loader's id<->name map. ----
    const FID_TITLE: u64 = 1;
    const FID_AUTHOR: u64 = 2;
    const FID_SUBJECT: u64 = 3;
    const FID_CREATOR: u64 = 4;
    const FID_PRODUCER: u64 = 5;
    const FID_CREATION_DATE: u64 = 6;
    const FID_MOD_DATE: u64 = 7;
    const FID_PAGE_COUNT: u64 = 8;
    const FID_PDF_VERSION: u64 = 9;
    const FID_IS_ENCRYPTED: u64 = 10;
    const FID_KEYWORDS: u64 = 11;
    const FID_ALL: u64 = 12;
    const FID_VERSION: u64 = 13;

    struct Ext;

    // ---- Input coercion ----
    //
    // BLOB is the canonical PDF carrier; TEXT is accepted too so
    // callers who've already round-tripped a PDF through TEXT columns
    // (latin-1 / lossy UTF-8) don't need an explicit CAST. We re-bytes
    // the TEXT view rather than re-decoding it as UTF-8 — PDF bytes
    // are NOT UTF-8 in general.
    fn opt_bytes(args: &[SqlValue]) -> Option<Vec<u8>> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// Full lopdf parse. Returns `None` on any error (corrupt xref,
    /// truncated body, unknown filter, etc.) — preserving the
    /// NULL-on-fail contract. lopdf::Document::load_mem handles the
    /// common cases (linearized PDFs, %%EOF tolerance) gracefully.
    fn parse(bytes: &[u8]) -> Option<Document> {
        Document::load_mem(bytes).ok()
    }

    /// Pull a string-typed /Info dictionary entry. Returns None when
    /// /Info is missing (PDFs without metadata) or the key is absent.
    /// Handles both literal PDFDocEncoding strings and UTF-16 BE
    /// strings (PDF 1.7 §7.9.2.2 — the `<FEFF ...>` BOM prefix marks
    /// the latter).
    fn info_string(doc: &Document, key: &[u8]) -> Option<String> {
        // Document::trailer.get(b"Info") -> ObjectId reference. Some
        // PDFs put the value inline (rare) — handle the reference case
        // explicitly and fall through on inline.
        let info_dict = if let Ok(id) = doc.trailer.get(b"Info").and_then(Object::as_reference) {
            doc.get_dictionary(id).ok()?
        } else if let Ok(d) = doc.trailer.get(b"Info").and_then(Object::as_dict) {
            d
        } else {
            return None;
        };
        let field = info_dict.get(key).ok()?;
        decode_pdf_string(field)
    }

    /// Decode a PDF string Object as Rust UTF-8. Handles:
    ///   - hex strings (Object::String with HEX format)
    ///   - literal strings (Object::String with LITERAL format)
    ///   - UTF-16 BE detection via FEFF BOM
    ///   - direct PDFDocEncoding -> UTF-8 (best-effort: ASCII subset
    ///     is identical; higher bytes are passed as Latin-1 since
    ///     full PDFDocEncoding requires a 256-entry table this
    ///     extension intentionally avoids carrying)
    fn decode_pdf_string(obj: &Object) -> Option<String> {
        let bytes = obj.as_str().ok()?;
        if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
            // UTF-16 BE, BOM included. PDF spec stores text strings
            // this way when they contain non-PDFDocEncoding chars.
            let payload = &bytes[2..];
            if payload.len() % 2 != 0 {
                return None;
            }
            let mut out = String::with_capacity(payload.len() / 2);
            for pair in payload.chunks_exact(2) {
                let cu = ((pair[0] as u16) << 8) | (pair[1] as u16);
                if let Some(c) = char::from_u32(cu as u32) {
                    out.push(c);
                }
                // Surrogate pairs split across iterations would need
                // proper decoding; lone surrogates we silently drop
                // (out-of-band Unicode is rare in PDF /Info strings).
            }
            return Some(out);
        }
        // Latin-1 / ASCII path. PDFDocEncoding is mostly Latin-1 in
        // the printable range; non-ASCII bytes are lossy here but
        // round-trip the common cases (English titles, ASCII names).
        let s: String = bytes.iter().map(|&b| b as char).collect();
        Some(s)
    }

    /// PDF /Info date strings look like `D:YYYYMMDDHHmmSSOHH'mm'` per
    /// the spec (§7.9.4). Rewrite into ISO 8601 when the shape is
    /// recognisable; otherwise return whatever was stored, trimmed.
    ///
    /// Examples:
    ///   D:20240115103045+05'00'  -> 2024-01-15T10:30:45+05:00
    ///   D:20240115103045Z        -> 2024-01-15T10:30:45Z
    ///   D:20240115103045         -> 2024-01-15T10:30:45
    fn pdf_date_to_iso(raw: &str) -> String {
        let s = raw.strip_prefix("D:").unwrap_or(raw);
        let b = s.as_bytes();
        if b.len() < 14 || !b[..14].iter().all(|c| c.is_ascii_digit()) {
            return raw.trim().to_string();
        }
        let yyyy = &s[0..4];
        let mm = &s[4..6];
        let dd = &s[6..8];
        let hh = &s[8..10];
        let mi = &s[10..12];
        let ss = &s[12..14];
        let mut out = format!("{yyyy}-{mm}-{dd}T{hh}:{mi}:{ss}");
        // Optional timezone tail: 'Z' or +HH'mm' or -HH'mm'.
        let tail = &s[14..];
        if tail.starts_with('Z') {
            out.push('Z');
        } else if (tail.starts_with('+') || tail.starts_with('-')) && tail.len() >= 3 {
            // Extract HH; minutes are after a quote.
            out.push(tail.chars().next().unwrap());
            out.push_str(&tail[1..3]);
            if let Some(mm_off) = tail
                .splitn(3, '\'')
                .nth(1)
                .filter(|s| s.len() >= 2 && s[..2].chars().all(|c| c.is_ascii_digit()))
            {
                out.push(':');
                out.push_str(&mm_off[..2]);
            } else {
                out.push_str(":00");
            }
        }
        out
    }

    /// Header-scan fallback: extract the `%PDF-x.y` version from the
    /// first 1024 bytes without a full parse. Useful for truncated
    /// PDFs (acceptance §1: "truncated PDF — version still extracts").
    /// Returns None if no `%PDF-` marker is found.
    fn header_version_scan(bytes: &[u8]) -> Option<String> {
        let window = &bytes[..bytes.len().min(1024)];
        // PDF spec: header is `%PDF-x.y` starting at byte 0 (allowed
        // up to a small offset for byte-order marks or shebangs). We
        // search rather than slice [0..] for tolerance.
        let needle = b"%PDF-";
        let idx = window.windows(needle.len()).position(|w| w == needle)?;
        let after = &window[idx + needle.len()..];
        // Read up to 5 bytes for "x.yz" (PDF 2.0 still fits in 3).
        let take = after.len().min(5);
        let chunk = core::str::from_utf8(&after[..take]).ok()?;
        // Pull leading "<digit>.<digit>(<digit>)*" prefix.
        let mut end = 0;
        let bytes = chunk.as_bytes();
        // Require digit
        if bytes.is_empty() || !bytes[0].is_ascii_digit() {
            return None;
        }
        end += 1;
        // Optional .digit(s)
        if bytes.len() > end && bytes[end] == b'.' {
            end += 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
        }
        Some(chunk[..end].to_string())
    }

    /// Header scan for the `/Encrypt` entry without a full parse.
    /// Used as a fallback for PDFs where lopdf decryption refuses
    /// to load but we still want the encrypted-flag answer to be 1.
    /// Conservative — we look for the literal byte sequence in the
    /// trailer region (last 4KB). False positives possible but rare
    /// (the token `/Encrypt` is reserved by the PDF spec).
    fn header_is_encrypted(bytes: &[u8]) -> bool {
        let n = bytes.len();
        // Search the whole file when small; the last 8KB when large.
        // The trailer / xref lives near EOF in non-linearized PDFs;
        // for linearized PDFs the first hint sits in the linearization
        // dict near the start, so the small-file path covers both.
        let window = if n <= 16 * 1024 {
            bytes
        } else {
            &bytes[n - 8192..]
        };
        let needle = b"/Encrypt";
        window.windows(needle.len()).any(|w| w == needle)
    }

    fn page_count(doc: &Document) -> Option<i64> {
        // lopdf's get_pages() walks /Catalog -> /Pages tree and returns
        // the leaf page object IDs. Length is the page count. Returns
        // an empty map if the catalog is missing — we collapse to None
        // in that case rather than reporting 0 (callers can't
        // distinguish "no metadata" from "empty PDF" otherwise).
        let pages = doc.get_pages();
        if pages.is_empty() {
            None
        } else {
            Some(pages.len() as i64)
        }
    }

    fn is_encrypted_doc(doc: &Document) -> bool {
        // lopdf's Document::is_encrypted just checks for /Encrypt in
        // trailer; matches the byte-scan path.
        doc.is_encrypted()
    }

    /// JSON-escape a string into an output buffer. ASCII control
    /// characters are emitted as \u00XX; backslash + quote are
    /// escaped; everything else passes through (we trust the caller
    /// has already decoded into Rust UTF-8).
    fn json_string(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    /// Append `"key":<json-value>` (skip-if-None semantics with a
    /// shared leading-comma trick — caller passes &mut bool first).
    fn append_kv_str(out: &mut String, first: &mut bool, key: &str, value: Option<&str>) {
        if let Some(v) = value {
            if !*first {
                out.push(',');
            }
            *first = false;
            json_string(out, key);
            out.push(':');
            json_string(out, v);
        }
    }

    fn append_kv_int(out: &mut String, first: &mut bool, key: &str, value: Option<i64>) {
        if let Some(v) = value {
            if !*first {
                out.push(',');
            }
            *first = false;
            json_string(out, key);
            out.push(':');
            let _ = write!(out, "{v}");
        }
    }

    fn append_kv_bool(out: &mut String, first: &mut bool, key: &str, value: bool) {
        if !*first {
            out.push(',');
        }
        *first = false;
        json_string(out, key);
        out.push(':');
        out.push_str(if value { "1" } else { "0" });
    }

    fn build_all_json(bytes: &[u8]) -> String {
        // pdf_all has to work even when lopdf fails — we want header
        // version + encryption hint to surface in that case. So build
        // the JSON from whatever we can extract, layer by layer.
        let mut out = String::from("{");
        let mut first = true;
        let doc = parse(bytes);
        if let Some(d) = &doc {
            append_kv_str(&mut out, &mut first, "title", info_string(d, b"Title").as_deref());
            append_kv_str(
                &mut out,
                &mut first,
                "author",
                info_string(d, b"Author").as_deref(),
            );
            append_kv_str(
                &mut out,
                &mut first,
                "subject",
                info_string(d, b"Subject").as_deref(),
            );
            append_kv_str(
                &mut out,
                &mut first,
                "creator",
                info_string(d, b"Creator").as_deref(),
            );
            append_kv_str(
                &mut out,
                &mut first,
                "producer",
                info_string(d, b"Producer").as_deref(),
            );
            append_kv_str(
                &mut out,
                &mut first,
                "keywords",
                info_string(d, b"Keywords").as_deref(),
            );
            if let Some(s) = info_string(d, b"CreationDate") {
                let iso = pdf_date_to_iso(&s);
                append_kv_str(&mut out, &mut first, "creation_date", Some(&iso));
            }
            if let Some(s) = info_string(d, b"ModDate") {
                let iso = pdf_date_to_iso(&s);
                append_kv_str(&mut out, &mut first, "mod_date", Some(&iso));
            }
            if let Some(n) = page_count(d) {
                append_kv_int(&mut out, &mut first, "page_count", Some(n));
            }
            // lopdf strips the trailing `\0` from version for us; pass
            // it through. Empty string means lopdf didn't find one,
            // fall back to header scan below.
            if !d.version.is_empty() {
                append_kv_str(&mut out, &mut first, "pdf_version", Some(&d.version));
            } else if let Some(v) = header_version_scan(bytes) {
                append_kv_str(&mut out, &mut first, "pdf_version", Some(&v));
            }
            append_kv_bool(&mut out, &mut first, "is_encrypted", is_encrypted_doc(d));
        } else {
            // Parse failed entirely. Still try the header scan.
            if let Some(v) = header_version_scan(bytes) {
                append_kv_str(&mut out, &mut first, "pdf_version", Some(&v));
            }
            // Encryption hint via byte scan.
            append_kv_bool(&mut out, &mut first, "is_encrypted", header_is_encrypted(bytes));
        }
        out.push('}');
        out
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
                name: "pdf-meta".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TITLE, "pdf_title", 1, det),
                    s(FID_AUTHOR, "pdf_author", 1, det),
                    s(FID_SUBJECT, "pdf_subject", 1, det),
                    s(FID_CREATOR, "pdf_creator", 1, det),
                    s(FID_PRODUCER, "pdf_producer", 1, det),
                    s(FID_CREATION_DATE, "pdf_creation_date", 1, det),
                    s(FID_MOD_DATE, "pdf_mod_date", 1, det),
                    s(FID_PAGE_COUNT, "pdf_page_count", 1, det),
                    s(FID_PDF_VERSION, "pdf_pdf_version", 1, det),
                    s(FID_IS_ENCRYPTED, "pdf_is_encrypted", 1, det),
                    s(FID_KEYWORDS, "pdf_keywords", 1, det),
                    s(FID_ALL, "pdf_all", 1, det),
                    s(FID_VERSION, "pdf_meta_version", 0, det),
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
                preferred_prefix: Some("pdf_meta".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.pdf_meta".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(format!(
                    "lopdf 0.34; extension {}",
                    env!("CARGO_PKG_VERSION")
                )));
            }

            let Some(bytes) = opt_bytes(&args) else {
                return Ok(SqlValue::Null);
            };

            // pdf_pdf_version + pdf_is_encrypted + pdf_all are happy
            // with a parse failure — they fall back to byte scans.
            // Handle them BEFORE the parse() short-circuit.
            match func_id {
                FID_PDF_VERSION => {
                    // Prefer the parsed value when available; fall
                    // back to the header scan otherwise.
                    if let Some(d) = parse(&bytes) {
                        if !d.version.is_empty() {
                            return Ok(SqlValue::Text(d.version));
                        }
                    }
                    return match header_version_scan(&bytes) {
                        Some(v) => Ok(SqlValue::Text(v)),
                        None => Ok(SqlValue::Null),
                    };
                }
                FID_IS_ENCRYPTED => {
                    // Parsed-doc check first (rules out false-positive
                    // /Encrypt-in-content-stream hits); fall back to
                    // byte scan when the parse fails.
                    if let Some(d) = parse(&bytes) {
                        return Ok(SqlValue::Integer(if is_encrypted_doc(&d) { 1 } else { 0 }));
                    }
                    // Random / non-PDF bytes get NULL, not 0. The
                    // contract is "non-PDF blob -> NULL on each fn"
                    // (PLAN-more-extensions-4 §1). Use the header
                    // scan to gate: only emit a 0/1 answer when this
                    // really does look like a PDF.
                    if header_version_scan(&bytes).is_none() {
                        return Ok(SqlValue::Null);
                    }
                    return Ok(SqlValue::Integer(if header_is_encrypted(&bytes) { 1 } else { 0 }));
                }
                FID_ALL => {
                    // pdf_all is the only fn that intentionally emits a
                    // partial JSON object on failure (header version +
                    // encryption hint). Other funcs return NULL.
                    if parse(&bytes).is_none() && header_version_scan(&bytes).is_none() {
                        return Ok(SqlValue::Null);
                    }
                    return Ok(SqlValue::Text(build_all_json(&bytes)));
                }
                _ => {}
            }

            // Remaining functions REQUIRE a successful full parse.
            let Some(doc) = parse(&bytes) else {
                return Ok(SqlValue::Null);
            };

            match func_id {
                FID_TITLE => match info_string(&doc, b"Title") {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_AUTHOR => match info_string(&doc, b"Author") {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_SUBJECT => match info_string(&doc, b"Subject") {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_CREATOR => match info_string(&doc, b"Creator") {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_PRODUCER => match info_string(&doc, b"Producer") {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_CREATION_DATE => match info_string(&doc, b"CreationDate") {
                    Some(s) => Ok(SqlValue::Text(pdf_date_to_iso(&s))),
                    None => Ok(SqlValue::Null),
                },
                FID_MOD_DATE => match info_string(&doc, b"ModDate") {
                    Some(s) => Ok(SqlValue::Text(pdf_date_to_iso(&s))),
                    None => Ok(SqlValue::Null),
                },
                FID_PAGE_COUNT => match page_count(&doc) {
                    Some(n) => Ok(SqlValue::Integer(n)),
                    None => Ok(SqlValue::Null),
                },
                FID_KEYWORDS => match info_string(&doc, b"Keywords") {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                other => Err(format!("pdf-meta: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
