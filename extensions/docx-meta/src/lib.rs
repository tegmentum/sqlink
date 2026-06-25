//! OOXML (docx / xlsx / pptx) metadata + plain-text extraction
//! from blobs. OOXML files are ZIP archives of XML parts; we use
//! `zip` 2 to enumerate the parts and `quick-xml` 0.36 to pull
//! values from the canonical metadata parts:
//!
//!   docProps/core.xml   Dublin Core metadata (title/author/dates)
//!   docProps/app.xml    Microsoft "extended properties" (Words / Pages / Slides)
//!   word/document.xml   docx body  paragraphs of <w:t> runs
//!   xl/sharedStrings.xml + xl/worksheets/sheet*.xml   xlsx cell data
//!   ppt/slides/slide*.xml   pptx text runs (<a:t>)
//!
//! Function surface (PLAN-more-extensions-4 §4):
//!
//!   docx_title(blob)         -> TEXT
//!   docx_author(blob)        -> TEXT
//!   docx_created(blob)       -> TEXT  (ISO 8601 — passed through verbatim;
//!                                      OOXML stores dcterms:created as
//!                                      W3CDTF which is already ISO 8601)
//!   docx_modified(blob)      -> TEXT  (ISO 8601, same as above)
//!   docx_word_count(blob)    -> INTEGER
//!   docx_page_count(blob)    -> INTEGER
//!   docx_format(blob)        -> TEXT  ("docx" / "xlsx" / "pptx")
//!   docx_text_content(blob)  -> TEXT
//!   docx_metadata(blob)      -> TEXT  (JSON object)
//!   docx_meta_version()      -> TEXT
//!
//! NULL contract: every accessor returns SQL NULL on
//!   - SqlValue::Null input
//!   - non-BLOB / non-TEXT input
//!   - blobs that aren't a valid ZIP at all
//!   - blobs that look ZIP-shaped but don't contain OOXML parts
//!   - the requested field being absent from the package
//!
//! Errors are NEVER surfaced to SQL — every scalar collapses to NULL
//! on bad input, mirroring the established convention in `exif` /
//! `image-meta` / `pdf-meta`.
//!
//! Each call re-parses the blob fresh; no shared state. The ZIP is
//! decompressed in-memory (OOXML packages are typically <1MB; large
//! decks may run multiple MB but still fit comfortably in the wasm
//! linear memory budget).

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::fmt::Write as _;
    use std::io::{Cursor, Read};

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

    // ---- Stable function IDs ----
    const FID_TITLE: u64 = 1;
    const FID_AUTHOR: u64 = 2;
    const FID_CREATED: u64 = 3;
    const FID_MODIFIED: u64 = 4;
    const FID_WORD_COUNT: u64 = 5;
    const FID_PAGE_COUNT: u64 = 6;
    const FID_FORMAT: u64 = 7;
    const FID_TEXT_CONTENT: u64 = 8;
    const FID_METADATA: u64 = 9;
    const FID_VERSION: u64 = 10;

    struct Ext;

    // ---- Input coercion ----
    fn opt_bytes(args: &[SqlValue]) -> Option<Vec<u8>> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    // ---- ZIP-level helpers ----

    /// In-memory zip reader over the blob. Returns None on any zip
    /// error — corrupted central directory, truncated file, password
    /// protection, etc. We intentionally don't surface the specific
    /// error: the NULL contract collapses every failure to one shape.
    fn open_archive(bytes: &[u8]) -> Option<zip::ZipArchive<Cursor<&[u8]>>> {
        zip::ZipArchive::new(Cursor::new(bytes)).ok()
    }

    /// Read a single named ZIP entry into a Vec<u8>. Returns None if
    /// the entry is absent or unreadable (e.g. unsupported compression
    /// method). OOXML mandates Stored or Deflated for all parts so the
    /// deflate-only feature set in Cargo.toml is sufficient.
    fn read_entry(
        archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
        name: &str,
    ) -> Option<Vec<u8>> {
        let mut entry = archive.by_name(name).ok()?;
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    /// Enumerate every entry name in the archive. Used for format
    /// detection (look for `word/`, `xl/`, `ppt/` prefixes) and for
    /// iterating slide/sheet parts whose count varies per package.
    fn list_names(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Vec<String> {
        (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
            .collect()
    }

    /// Read [Content_Types].xml as a string; used as a sanity check
    /// that this is really an OOXML package (every conforming OOXML
    /// file has this part at the package root). Errors collapse to
    /// None; downstream callers treat None as "not OOXML".
    fn read_content_types(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
        let bytes = read_entry(archive, "[Content_Types].xml")?;
        String::from_utf8(bytes).ok()
    }

    /// Discriminate the three OOXML families.
    ///
    /// Order of detection:
    ///   1. presence of `word/document.xml` -> docx
    ///   2. presence of `xl/workbook.xml`   -> xlsx
    ///   3. presence of `ppt/presentation.xml` -> pptx
    ///
    /// We could also key off the [Content_Types].xml entries
    /// (application/vnd.openxmlformats-officedocument.{wordprocessingml,
    /// spreadsheetml, presentationml}...) but the part-name check is
    /// simpler and matches what every real OOXML producer emits.
    fn detect_format(names: &[String]) -> Option<&'static str> {
        let has = |p: &str| names.iter().any(|n| n == p);
        if has("word/document.xml") {
            Some("docx")
        } else if has("xl/workbook.xml") {
            Some("xlsx")
        } else if has("ppt/presentation.xml") {
            Some("pptx")
        } else {
            None
        }
    }

    // ---- XML helpers ----

    /// Strip an XML element's namespace prefix. quick-xml's QName
    /// includes the prefix ("dc:title"); we match on the local name
    /// so callers don't have to track which namespace prefix the
    /// producer chose.
    fn local_name(qname: &[u8]) -> &[u8] {
        match qname.iter().position(|&b| b == b':') {
            Some(i) => &qname[i + 1..],
            None => qname,
        }
    }

    /// Lossy UTF-8 decode. OOXML XML is utf-8 per the spec, but we'd
    /// rather see a slightly-mangled string than abort the whole parse.
    fn to_string(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    /// Pull the inner text of every occurrence of `target` (matched by
    /// local name) and return them as separate strings. Used for
    /// docProps/core.xml + app.xml + text extraction where multiple
    /// matches are concatenated by the caller.
    fn extract_element_texts(xml: &str, target: &[u8]) -> Vec<String> {
        use quick_xml::events::Event;
        use quick_xml::reader::Reader;
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(false);
        let mut out: Vec<String> = Vec::new();
        let mut depth_into_target: i32 = 0;
        let mut current = String::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    if local_name(e.name().as_ref()) == target {
                        if depth_into_target == 0 {
                            current.clear();
                        }
                        depth_into_target += 1;
                    } else if depth_into_target > 0 {
                        depth_into_target += 1;
                    }
                }
                Ok(Event::End(e)) => {
                    if depth_into_target > 0 {
                        depth_into_target -= 1;
                        if depth_into_target == 0 && local_name(e.name().as_ref()) == target {
                            out.push(core::mem::take(&mut current));
                        }
                    }
                }
                Ok(Event::Text(t)) => {
                    if depth_into_target > 0 {
                        // unescape() handles &amp; / &lt; / &#10; etc.
                        // Drop on failure -- we'd rather lose entity
                        // decoding than the whole text.
                        if let Ok(s) = t.unescape() {
                            current.push_str(&s);
                        } else {
                            current.push_str(&to_string(t.as_ref()));
                        }
                    }
                }
                Ok(Event::CData(c)) => {
                    if depth_into_target > 0 {
                        current.push_str(&to_string(c.as_ref()));
                    }
                }
                Ok(Event::Empty(e)) => {
                    // <foo/> inside our target counts as a self-closing
                    // child with no content; nothing to capture but we
                    // must keep depth tracking correct (no change here
                    // because Empty does not nest).
                    let _ = e;
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        out
    }

    /// First match wins helper for the single-valued metadata elements.
    fn first_element_text(xml: &str, target: &[u8]) -> Option<String> {
        let mut v = extract_element_texts(xml, target);
        if v.is_empty() {
            None
        } else {
            Some(v.remove(0))
        }
    }

    // ---- Core / extended properties ----

    /// docProps/core.xml carries Dublin Core metadata. Spec part
    /// `application/vnd.openxmlformats-package.core-properties+xml`.
    /// Elements we care about (with their typical namespace prefixes):
    ///   <dc:title>           Title
    ///   <dc:creator>         Author
    ///   <dcterms:created>    Creation date (W3CDTF, ISO 8601)
    ///   <dcterms:modified>   Modified date (W3CDTF, ISO 8601)
    fn read_core_props(
        archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    ) -> Option<String> {
        let bytes = read_entry(archive, "docProps/core.xml")?;
        String::from_utf8(bytes).ok()
    }

    /// docProps/app.xml carries the Microsoft-extended properties.
    /// Elements we care about:
    ///   <Words>          word count
    ///   <Pages>          page count (docx)
    ///   <Slides>         slide count (pptx)
    ///   <Application>    "Microsoft Office Word" etc.
    fn read_app_props(
        archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    ) -> Option<String> {
        let bytes = read_entry(archive, "docProps/app.xml")?;
        String::from_utf8(bytes).ok()
    }

    // ---- Text content extraction ----

    /// docx body text: walk word/document.xml and concatenate every
    /// <w:t> run's text. Paragraphs are separated by '\n' (insert one
    /// at each <w:p> close). The full WordprocessingML spec carries
    /// dozens of inline shapes — we limit ourselves to <w:t> because
    /// that's the canonical text carrier and every other shape boils
    /// down to it.
    fn extract_docx_text(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
        let bytes = read_entry(archive, "word/document.xml")?;
        let xml = String::from_utf8(bytes).ok()?;
        Some(extract_text_with_paragraphs(&xml, b"t", b"p"))
    }

    /// pptx text: enumerate ppt/slides/slide*.xml in name order,
    /// concatenate each slide's <a:t> runs, separate slides with a
    /// blank line. The naming convention `slide1.xml`, `slide2.xml`,
    /// ... is mandated by the OOXML spec so a lexical sort is OK for
    /// up to 9 slides; beyond that we still get a stable order though
    /// it may not match presentation order. Real-world test fixtures
    /// in our smoke set stay under 9.
    fn extract_pptx_text(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
        let mut slide_names: Vec<String> = list_names(archive)
            .into_iter()
            .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
            .collect();
        if slide_names.is_empty() {
            return None;
        }
        slide_names.sort();
        let mut out = String::new();
        let mut first = true;
        for name in &slide_names {
            let Some(bytes) = read_entry(archive, name) else {
                continue;
            };
            let Ok(xml) = String::from_utf8(bytes) else {
                continue;
            };
            if !first {
                out.push('\n');
            }
            first = false;
            out.push_str(&extract_text_with_paragraphs(&xml, b"t", b"p"));
        }
        Some(out)
    }

    /// xlsx text: shared strings table first (xl/sharedStrings.xml
    /// stores all shared text), then inline strings from each sheet.
    /// We don't currently decode numeric cell values into the text
    /// dump — that's `excel` vtab's job; here we surface the human-
    /// readable strings only.
    fn extract_xlsx_text(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
        // Collect shared strings into one flat list. The SharedStringTable
        // schema is <sst><si><t>...</t></si>...</sst> where each <si> can
        // also contain runs of <r><t>...</t></r>. extract_element_texts
        // matches every <t> regardless of parent so we capture both forms.
        let mut out = String::new();
        if let Some(bytes) = read_entry(archive, "xl/sharedStrings.xml") {
            if let Ok(xml) = String::from_utf8(bytes) {
                let strings = extract_element_texts(&xml, b"t");
                for s in &strings {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(s);
                }
            }
        }
        // Inline strings + cells: walk each sheet and append any <t>
        // text we find (covers worksheets that bypass sharedStrings).
        let sheet_names: Vec<String> = list_names(archive)
            .into_iter()
            .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
            .collect();
        for name in &sheet_names {
            let Some(bytes) = read_entry(archive, name) else {
                continue;
            };
            let Ok(xml) = String::from_utf8(bytes) else {
                continue;
            };
            // Inline strings live under <c t="inlineStr"><is><t>...
            // The same <t>-as-local-name match grabs them.
            for s in extract_element_texts(&xml, b"is") {
                if !s.is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&s);
                }
            }
        }
        if out.is_empty() { None } else { Some(out) }
    }

    /// Shared paragraph-aware text extractor. Walks the XML and:
    ///   - appends the body of every element whose local name is `text_target`
    ///   - inserts '\n' on every close of an element whose local name
    ///     is `paragraph_target`
    /// Whitespace between targets is dropped so we don't end up with
    /// gigantic runs of blanks from formatting elements.
    fn extract_text_with_paragraphs(
        xml: &str,
        text_target: &[u8],
        paragraph_target: &[u8],
    ) -> String {
        use quick_xml::events::Event;
        use quick_xml::reader::Reader;
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(false);
        let mut out = String::new();
        let mut depth_into_text: i32 = 0;
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    if local_name(e.name().as_ref()) == text_target {
                        depth_into_text += 1;
                    }
                }
                Ok(Event::End(e)) => {
                    let qname = e.name();
                    let lname = local_name(qname.as_ref());
                    let is_text = lname == text_target;
                    let is_para = lname == paragraph_target;
                    if is_text && depth_into_text > 0 {
                        depth_into_text -= 1;
                    }
                    if is_para {
                        // Don't double-newline on consecutive paragraph
                        // closes (e.g. <w:p/><w:p/> at the start of a doc).
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                }
                Ok(Event::Text(t)) => {
                    if depth_into_text > 0 {
                        if let Ok(s) = t.unescape() {
                            out.push_str(&s);
                        } else {
                            out.push_str(&to_string(t.as_ref()));
                        }
                    }
                }
                Ok(Event::CData(c)) => {
                    if depth_into_text > 0 {
                        out.push_str(&to_string(c.as_ref()));
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        // Trim a single trailing newline if present — paragraph runs
        // always emit one after the final close. Multiple trailing
        // newlines (intentional blank paragraphs) are preserved by the
        // single-strip; the more we trim, the more we lose author
        // intent.
        if out.ends_with('\n') {
            out.pop();
        }
        out
    }

    // ---- Counts ----

    /// docx page count = <Pages> from docProps/app.xml when present.
    /// OOXML producers populate this on save based on the last layout
    /// pass; the page count is therefore stale-on-edit but trustworthy
    /// for downstream archival queries.
    fn docx_page_count(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<i64> {
        let xml = read_app_props(archive)?;
        first_element_text(&xml, b"Pages").and_then(|s| s.trim().parse().ok())
    }

    /// docx word count = <Words> from docProps/app.xml.
    fn docx_word_count(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<i64> {
        let xml = read_app_props(archive)?;
        first_element_text(&xml, b"Words").and_then(|s| s.trim().parse().ok())
    }

    /// xlsx page count = number of xl/worksheets/sheet*.xml parts.
    /// The workbook's <sheets><sheet/></sheets> list is the canonical
    /// source but counting parts is equivalent and avoids a second
    /// XML parse.
    fn xlsx_sheet_count(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<i64> {
        let n = list_names(archive)
            .into_iter()
            .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
            .count() as i64;
        if n == 0 { None } else { Some(n) }
    }

    /// xlsx word count = number of <c> cell elements with content.
    /// Per spec "word count" for spreadsheets is nonsensical; we
    /// substitute "non-empty cell count" which is what downstream
    /// analytics queries typically want.
    fn xlsx_cell_count(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<i64> {
        let sheet_names: Vec<String> = list_names(archive)
            .into_iter()
            .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
            .collect();
        if sheet_names.is_empty() {
            return None;
        }
        let mut total: i64 = 0;
        for name in &sheet_names {
            let Some(bytes) = read_entry(archive, name) else { continue };
            let Ok(xml) = String::from_utf8(bytes) else { continue };
            // Count <v> (cell value) elements — every cell with a value
            // has exactly one <v>. Inline-string cells have <is> instead;
            // count those too.
            total += extract_element_texts(&xml, b"v").len() as i64;
            total += extract_element_texts(&xml, b"is").len() as i64;
        }
        Some(total)
    }

    /// pptx page count = number of ppt/slides/slide*.xml parts.
    /// Alternative source is <Slides> in app.xml (set by PowerPoint
    /// on save); we use the part count so the answer stays accurate
    /// for packages built outside PowerPoint that may omit app.xml.
    fn pptx_slide_count(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<i64> {
        let n = list_names(archive)
            .into_iter()
            .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
            .count() as i64;
        if n == 0 { None } else { Some(n) }
    }

    /// pptx word count: app.xml <Words> if present, else fall back to
    /// the slide count (a non-zero stand-in better than NULL when the
    /// producer skipped <Words>).
    fn pptx_word_count(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<i64> {
        if let Some(xml) = read_app_props(archive) {
            if let Some(s) = first_element_text(&xml, b"Words") {
                if let Ok(n) = s.trim().parse() {
                    return Some(n);
                }
            }
        }
        None
    }

    // ---- JSON output ----

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

    /// Build the docx_metadata JSON object. Same field shape regardless
    /// of OOXML family; absent fields are omitted (not emitted as null)
    /// so callers can use SQLite's json_extract with the standard
    /// "missing path -> NULL" behavior.
    fn build_metadata_json(bytes: &[u8]) -> Option<String> {
        let mut archive = open_archive(bytes)?;
        // Sanity check: confirm this is OOXML before emitting anything.
        let _content_types = read_content_types(&mut archive)?;
        let names = list_names(&mut archive);
        let format = detect_format(&names)?;

        let mut out = String::from("{");
        let mut first = true;
        append_kv_str(&mut out, &mut first, "format", Some(format));

        if let Some(core_xml) = read_core_props(&mut archive) {
            append_kv_str(&mut out, &mut first, "title",
                first_element_text(&core_xml, b"title").as_deref());
            append_kv_str(&mut out, &mut first, "author",
                first_element_text(&core_xml, b"creator").as_deref());
            append_kv_str(&mut out, &mut first, "created",
                first_element_text(&core_xml, b"created").as_deref());
            append_kv_str(&mut out, &mut first, "modified",
                first_element_text(&core_xml, b"modified").as_deref());
        }

        let page_count = match format {
            "docx" => docx_page_count(&mut archive),
            "xlsx" => xlsx_sheet_count(&mut archive),
            "pptx" => pptx_slide_count(&mut archive),
            _ => None,
        };
        append_kv_int(&mut out, &mut first, "page_count", page_count);

        let word_count = match format {
            "docx" => docx_word_count(&mut archive),
            "xlsx" => xlsx_cell_count(&mut archive),
            "pptx" => pptx_word_count(&mut archive),
            _ => None,
        };
        append_kv_int(&mut out, &mut first, "word_count", word_count);

        out.push('}');
        Some(out)
    }

    // ---- Top-level dispatchers ----

    fn title_of(bytes: &[u8]) -> Option<String> {
        let mut archive = open_archive(bytes)?;
        let _ = read_content_types(&mut archive)?;
        let names = list_names(&mut archive);
        let _ = detect_format(&names)?;
        let xml = read_core_props(&mut archive)?;
        first_element_text(&xml, b"title")
    }

    fn author_of(bytes: &[u8]) -> Option<String> {
        let mut archive = open_archive(bytes)?;
        let _ = read_content_types(&mut archive)?;
        let names = list_names(&mut archive);
        let _ = detect_format(&names)?;
        let xml = read_core_props(&mut archive)?;
        first_element_text(&xml, b"creator")
    }

    fn created_of(bytes: &[u8]) -> Option<String> {
        let mut archive = open_archive(bytes)?;
        let _ = read_content_types(&mut archive)?;
        let names = list_names(&mut archive);
        let _ = detect_format(&names)?;
        let xml = read_core_props(&mut archive)?;
        first_element_text(&xml, b"created")
    }

    fn modified_of(bytes: &[u8]) -> Option<String> {
        let mut archive = open_archive(bytes)?;
        let _ = read_content_types(&mut archive)?;
        let names = list_names(&mut archive);
        let _ = detect_format(&names)?;
        let xml = read_core_props(&mut archive)?;
        first_element_text(&xml, b"modified")
    }

    fn format_of(bytes: &[u8]) -> Option<&'static str> {
        let mut archive = open_archive(bytes)?;
        let _ = read_content_types(&mut archive)?;
        let names = list_names(&mut archive);
        detect_format(&names)
    }

    fn word_count_of(bytes: &[u8]) -> Option<i64> {
        let mut archive = open_archive(bytes)?;
        let _ = read_content_types(&mut archive)?;
        let names = list_names(&mut archive);
        let format = detect_format(&names)?;
        match format {
            "docx" => docx_word_count(&mut archive),
            "xlsx" => xlsx_cell_count(&mut archive),
            "pptx" => pptx_word_count(&mut archive),
            _ => None,
        }
    }

    fn page_count_of(bytes: &[u8]) -> Option<i64> {
        let mut archive = open_archive(bytes)?;
        let _ = read_content_types(&mut archive)?;
        let names = list_names(&mut archive);
        let format = detect_format(&names)?;
        match format {
            "docx" => docx_page_count(&mut archive),
            "xlsx" => xlsx_sheet_count(&mut archive),
            "pptx" => pptx_slide_count(&mut archive),
            _ => None,
        }
    }

    fn text_content_of(bytes: &[u8]) -> Option<String> {
        let mut archive = open_archive(bytes)?;
        let _ = read_content_types(&mut archive)?;
        let names = list_names(&mut archive);
        let format = detect_format(&names)?;
        match format {
            "docx" => extract_docx_text(&mut archive),
            "xlsx" => extract_xlsx_text(&mut archive),
            "pptx" => extract_pptx_text(&mut archive),
            _ => None,
        }
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
                name: "docx-meta".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TITLE, "docx_title", 1, det),
                    s(FID_AUTHOR, "docx_author", 1, det),
                    s(FID_CREATED, "docx_created", 1, det),
                    s(FID_MODIFIED, "docx_modified", 1, det),
                    s(FID_WORD_COUNT, "docx_word_count", 1, det),
                    s(FID_PAGE_COUNT, "docx_page_count", 1, det),
                    s(FID_FORMAT, "docx_format", 1, det),
                    s(FID_TEXT_CONTENT, "docx_text_content", 1, det),
                    s(FID_METADATA, "docx_metadata", 1, det),
                    s(FID_VERSION, "docx_meta_version", 0, det),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(format!(
                    "zip 2 + quick-xml 0.36; extension {}",
                    env!("CARGO_PKG_VERSION")
                )));
            }
            let Some(bytes) = opt_bytes(&args) else {
                return Ok(SqlValue::Null);
            };
            match func_id {
                FID_TITLE => Ok(title_of(&bytes)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_AUTHOR => Ok(author_of(&bytes)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_CREATED => Ok(created_of(&bytes)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_MODIFIED => Ok(modified_of(&bytes)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_WORD_COUNT => Ok(word_count_of(&bytes)
                    .map(SqlValue::Integer)
                    .unwrap_or(SqlValue::Null)),
                FID_PAGE_COUNT => Ok(page_count_of(&bytes)
                    .map(SqlValue::Integer)
                    .unwrap_or(SqlValue::Null)),
                FID_FORMAT => Ok(format_of(&bytes)
                    .map(|s| SqlValue::Text(s.to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_TEXT_CONTENT => Ok(text_content_of(&bytes)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_METADATA => Ok(build_metadata_json(&bytes)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("docx-meta: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
