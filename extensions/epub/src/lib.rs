//! EPUB e-book metadata extraction from a raw blob.
//!
//! An EPUB file is a ZIP archive whose root contains
//! `META-INF/container.xml`. That file points to a "Package Document"
//! (`.opf`) whose `<metadata>` block holds Dublin Core elements
//! describing the book. We unzip the blob in-memory, locate the OPF
//! path through `container.xml`, parse the OPF's `<metadata>` and
//! `<spine>` sections, and surface the fields as SQL scalars.
//!
//! Function surface (see Cargo.toml for the prose):
//!   epub_title(blob)          -> TEXT
//!   epub_authors(blob)        -> TEXT (JSON array)
//!   epub_language(blob)       -> TEXT
//!   epub_publisher(blob)      -> TEXT
//!   epub_published_date(blob) -> TEXT
//!   epub_identifier(blob)     -> TEXT
//!   epub_subjects(blob)       -> TEXT (JSON array)
//!   epub_chapter_count(blob)  -> INTEGER
//!   epub_metadata(blob)       -> TEXT (JSON object)
//!   epub_version()            -> TEXT
//!
//! NULL contract: every accessor returns SQL NULL on
//!   - SqlValue::Null input or non-BLOB/non-TEXT arg
//!   - blobs that fail to open as a ZIP archive (bad signature,
//!     truncated central directory, etc.)
//!   - ZIPs that lack META-INF/container.xml or whose container.xml
//!     doesn't point at a parseable OPF entry
//!   - the requested field being absent from a parsed OPF
//!
//! Errors are NEVER surfaced to SQL -- mirroring the `exif` /
//! `image-meta` convention. Each call re-parses the blob fresh; no
//! shared state.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;
    use std::io::{Cursor, Read};

    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

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

    // ---- Function IDs (stable; changing breaks the loader's id->name map). ----
    const FID_TITLE: u64 = 1;
    const FID_AUTHORS: u64 = 2;
    const FID_LANGUAGE: u64 = 3;
    const FID_PUBLISHER: u64 = 4;
    const FID_PUBLISHED_DATE: u64 = 5;
    const FID_IDENTIFIER: u64 = 6;
    const FID_SUBJECTS: u64 = 7;
    const FID_CHAPTER_COUNT: u64 = 8;
    const FID_METADATA: u64 = 9;
    const FID_VERSION: u64 = 10;

    struct Ext;

    // ---- Input coercion ----
    //
    // Per the NULL contract, BLOB / TEXT are the only acceptable arg-0
    // types. TEXT is treated as its raw UTF-8 byte view -- handy when
    // an EPUB has been hex-decoded into a TEXT column already.
    fn opt_bytes(args: &[SqlValue]) -> Option<Vec<u8>> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    // ---- Parsed metadata shape ----
    //
    // All fields are optional because the spec only mandates title +
    // language + identifier in practice -- the rest are commonly
    // omitted by self-published / converted books. `subjects` and
    // `authors` are Vecs because EPUB allows repeated `<dc:creator>`
    // and `<dc:subject>` elements (multi-author / multi-subject books).
    #[derive(Default, Clone, Debug)]
    struct EpubMeta {
        title: Option<String>,
        authors: Vec<String>,
        language: Option<String>,
        publisher: Option<String>,
        date: Option<String>,
        identifier: Option<String>,
        subjects: Vec<String>,
        description: Option<String>,
        rights: Option<String>,
        contributors: Vec<String>,
        chapter_count: u32,
        // EPUB package version ("2.0" / "3.0"); pulled from the
        // <package version="..."> attribute on the OPF root.
        pkg_version: Option<String>,
    }

    /// Top-level parse: blob -> EpubMeta. Returns `None` on any error
    /// -- bad ZIP, missing container.xml, malformed OPF -- preserving
    /// the NULL-on-fail contract.
    fn parse(bytes: &[u8]) -> Option<EpubMeta> {
        let cur = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cur).ok()?;

        // Step 1: locate the OPF path via META-INF/container.xml.
        // Per OCF spec, container.xml lives at exactly this path
        // (case-sensitive) and is the only "rootfile" discovery
        // mechanism. Missing container.xml -> not a valid EPUB.
        let opf_path = read_container_xml(&mut archive)?;

        // Step 2: read the OPF entry. The path inside container.xml
        // is relative to the archive root. We accept it as-is rather
        // than canonicalizing leading "./" -- ZIP entry names always
        // use '/' separators with no leading slash.
        let opf_bytes = read_entry(&mut archive, &opf_path)?;
        let opf_text = String::from_utf8_lossy(&opf_bytes);

        // Step 3: parse the OPF for <metadata> and <spine> contents.
        parse_opf(&opf_text)
    }

    /// Pull META-INF/container.xml from the archive and return the
    /// `full-path` attribute of its first `<rootfile>` element.
    /// Returns None if the file is missing, doesn't parse, or has no
    /// rootfile.
    fn read_container_xml(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
        let bytes = read_entry(archive, "META-INF/container.xml")?;
        let text = String::from_utf8_lossy(&bytes);
        let mut reader = Reader::from_str(&text);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf).ok()? {
                Event::Eof => return None,
                // `<rootfile full-path="..." />` is the canonical
                // shape; we accept either an Empty (self-closing)
                // or Start tag since spec-strict tools sometimes
                // emit `<rootfile ...></rootfile>`.
                Event::Empty(e) | Event::Start(e) => {
                    if local_name(e.name().as_ref()) == b"rootfile" {
                        for attr in e.attributes().with_checks(false).flatten() {
                            if attr.key.as_ref() == b"full-path" {
                                let v = attr.unescape_value().ok()?;
                                return Some(v.into_owned());
                            }
                        }
                    }
                }
                _ => {}
            }
            buf.clear();
        }
    }

    /// Read a named entry from the archive into a Vec. Returns None
    /// if the entry is missing or the decompress step fails.
    fn read_entry(
        archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
        name: &str,
    ) -> Option<Vec<u8>> {
        let mut entry = archive.by_name(name).ok()?;
        let mut out = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut out).ok()?;
        Some(out)
    }

    /// Strip an XML element's namespace prefix. quick-xml's `name()`
    /// returns the full QName (e.g. `dc:title`) -- we want the local
    /// part so namespace-prefixed OPF documents parse without us
    /// pinning to a specific xmlns.
    fn local_name(qname: &[u8]) -> &[u8] {
        match qname.iter().position(|&b| b == b':') {
            Some(i) => &qname[i + 1..],
            None => qname,
        }
    }

    /// Walk the OPF document and fill an `EpubMeta`. The grammar we
    /// recognize (loosely; we match by local-name regardless of
    /// namespace prefix):
    ///
    ///   <package version="..." ...>
    ///     <metadata>
    ///       <dc:title>...</dc:title>
    ///       <dc:creator>...</dc:creator>      (repeatable)
    ///       <dc:contributor>...</dc:contributor>  (repeatable)
    ///       <dc:language>...</dc:language>
    ///       <dc:publisher>...</dc:publisher>
    ///       <dc:date>...</dc:date>
    ///       <dc:identifier scheme="ISBN">...</dc:identifier>
    ///       <dc:subject>...</dc:subject>      (repeatable)
    ///       <dc:description>...</dc:description>
    ///       <dc:rights>...</dc:rights>
    ///     </metadata>
    ///     <spine>
    ///       <itemref idref="..." />            (repeatable)
    ///     </spine>
    ///   </package>
    ///
    /// Repeated single-cardinality fields (title/language/...) keep
    /// the first value, matching what most EPUB readers display.
    fn parse_opf(opf: &str) -> Option<EpubMeta> {
        let mut reader = Reader::from_str(opf);
        reader.config_mut().trim_text(true);

        let mut out = EpubMeta::default();
        // Which Dublin Core field we're currently accumulating into,
        // or None when between fields. Match by local-name only.
        let mut current: Option<&'static str> = None;
        // Text accumulator -- quick-xml can split one text node into
        // multiple events when entities or CDATA are interleaved.
        let mut text_buf = String::new();
        // True while inside the <metadata> container -- guards the
        // Dublin Core matchers so identically-named tags elsewhere
        // don't poison the metadata.
        let mut in_metadata = false;
        // True while inside <spine> -- bumps chapter_count per
        // <itemref> regardless of self-closing form.
        let mut in_spine = false;
        // For epub_identifier: capture the scheme attribute on the
        // <dc:identifier> tag so we can strip prefixes like
        // "urn:isbn:" if no scheme attr is present.
        // (Currently unused but reserved for richer parsing.)

        let mut buf = Vec::new();
        // True once we've seen the <package> root and captured its
        // `version` attribute. The OPF spec mandates exactly one
        // `<package>` element, so first-seen wins.
        let mut saw_package = false;

        loop {
            let event = reader.read_event_into(&mut buf).ok()?;
            match event {
                Event::Eof => break,
                Event::Start(e) => {
                    let name_owned = local_name(e.name().as_ref()).to_vec();
                    let name = name_owned.as_slice();
                    if !saw_package && name == b"package" {
                        saw_package = true;
                        for attr in e.attributes().with_checks(false).flatten() {
                            if attr.key.as_ref() == b"version" {
                                if let Ok(v) = attr.unescape_value() {
                                    out.pkg_version = Some(v.into_owned());
                                }
                            }
                        }
                    } else if name == b"metadata" {
                        in_metadata = true;
                    } else if name == b"spine" {
                        in_spine = true;
                    } else if name == b"itemref" && in_spine {
                        // <itemref> can be Empty or Start (some
                        // tools write `<itemref ...></itemref>`);
                        // count both shapes.
                        out.chapter_count += 1;
                    } else if in_metadata {
                        match name {
                            b"title" => { current = Some("title"); text_buf.clear(); }
                            b"creator" => { current = Some("creator"); text_buf.clear(); }
                            b"contributor" => { current = Some("contributor"); text_buf.clear(); }
                            b"language" => { current = Some("language"); text_buf.clear(); }
                            b"publisher" => { current = Some("publisher"); text_buf.clear(); }
                            b"date" => { current = Some("date"); text_buf.clear(); }
                            b"identifier" => { current = Some("identifier"); text_buf.clear(); }
                            b"subject" => { current = Some("subject"); text_buf.clear(); }
                            b"description" => { current = Some("description"); text_buf.clear(); }
                            b"rights" => { current = Some("rights"); text_buf.clear(); }
                            _ => {}
                        }
                    }
                }
                Event::Empty(e) => {
                    let name_owned = local_name(e.name().as_ref()).to_vec();
                    let name = name_owned.as_slice();
                    if name == b"itemref" && in_spine {
                        out.chapter_count += 1;
                    }
                }
                Event::Text(t) => {
                    if current.is_some() {
                        let s = t
                            .unescape()
                            .map(|s| s.into_owned())
                            .unwrap_or_else(|_| String::from_utf8_lossy(t.as_ref()).into_owned());
                        text_buf.push_str(&s);
                    }
                }
                Event::CData(t) => {
                    if current.is_some() {
                        text_buf.push_str(&String::from_utf8_lossy(t.as_ref()));
                    }
                }
                Event::End(e) => {
                    let name_owned = local_name(e.name().as_ref()).to_vec();
                    let name = name_owned.as_slice();
                    if name == b"metadata" {
                        in_metadata = false;
                        current = None;
                    } else if name == b"spine" {
                        in_spine = false;
                    } else if let Some(field) = current {
                        let v = text_buf.trim().to_string();
                        if !v.is_empty() {
                            store_field(&mut out, field, v);
                        }
                        current = None;
                    }
                }
                _ => {}
            }
            buf.clear();
        }

        // A successfully parsed OPF that has no <package> root means
        // we were handed something that wasn't an OPF after all
        // (random XML masquerading as EPUB's rootfile). Refuse.
        if !saw_package {
            return None;
        }
        Some(out)
    }

    /// Append captured text to the right slot in EpubMeta. Repeated
    /// `<dc:title>` etc. keep the first value, matching reader UX;
    /// repeated `<dc:creator>` / `<dc:contributor>` / `<dc:subject>`
    /// accumulate into their Vec.
    fn store_field(out: &mut EpubMeta, field: &str, value: String) {
        match field {
            "title" => {
                if out.title.is_none() {
                    out.title = Some(value);
                }
            }
            "creator" => out.authors.push(value),
            "contributor" => out.contributors.push(value),
            "language" => {
                if out.language.is_none() {
                    out.language = Some(value);
                }
            }
            "publisher" => {
                if out.publisher.is_none() {
                    out.publisher = Some(value);
                }
            }
            "date" => {
                if out.date.is_none() {
                    out.date = Some(value);
                }
            }
            "identifier" => {
                if out.identifier.is_none() {
                    out.identifier = Some(strip_id_scheme(&value));
                }
            }
            "subject" => out.subjects.push(value),
            "description" => {
                if out.description.is_none() {
                    out.description = Some(value);
                }
            }
            "rights" => {
                if out.rights.is_none() {
                    out.rights = Some(value);
                }
            }
            _ => {}
        }
    }

    /// Strip common URN scheme prefixes from a Dublin Core
    /// identifier. EPUB packagers stuff IDs into <dc:identifier>
    /// with various conventions:
    ///   urn:isbn:9780000000000  ->  9780000000000
    ///   urn:uuid:abc-...        ->  abc-...
    ///   isbn:9780000000000      ->  9780000000000
    /// Anything not matching a recognized scheme passes through
    /// untouched -- a bare ISBN, DOI, ARK, or proprietary id.
    fn strip_id_scheme(s: &str) -> String {
        let lower = s.to_ascii_lowercase();
        for pfx in ["urn:isbn:", "urn:uuid:", "urn:doi:", "isbn:", "uuid:", "doi:"] {
            if let Some(rest) = lower.strip_prefix(pfx) {
                // Use the original-case suffix; lowercasing UUIDs
                // is fine but lowercasing a DOI's case-preserving
                // segment is not.
                let cut = s.len() - rest.len();
                return s[cut..].to_string();
            }
        }
        s.to_string()
    }

    /// Try to normalize a `<dc:date>` to ISO 8601. The OPF spec
    /// (DCMI / W3CDTF profile) already mandates ISO 8601 -- but
    /// "many" tools emit US-style or European-style dates anyway.
    /// We pass through values that are already ISO 8601 (or that
    /// look ISO-8601-ish: YYYY-MM-DD, YYYY-MM, YYYY) and otherwise
    /// return the raw string unchanged. Better to hand callers
    /// whatever's there than to lose data on a heuristic miss.
    fn normalize_date(raw: &str) -> String {
        let s = raw.trim();
        // Fast accept: pure year, or YYYY-MM, or YYYY-MM-DD, or
        // YYYY-MM-DDTHH:MM... -- all already ISO 8601.
        if iso8601_shape(s) {
            return s.to_string();
        }
        s.to_string()
    }

    /// Lightweight check that a string starts YYYY-MM-DD or YYYY-MM
    /// or just YYYY. Returns false for anything that doesn't even
    /// begin with four ASCII digits. Used by `normalize_date` to
    /// decide "looks fine as-is".
    fn iso8601_shape(s: &str) -> bool {
        let b = s.as_bytes();
        if b.len() < 4 {
            return false;
        }
        if !(b[0].is_ascii_digit() && b[1].is_ascii_digit()
            && b[2].is_ascii_digit() && b[3].is_ascii_digit())
        {
            return false;
        }
        // year-only
        if b.len() == 4 { return true; }
        // year-month / year-month-day / datetime: next byte must be '-'
        b[4] == b'-'
    }

    // ---- JSON serialization ----
    //
    // We hand-roll JSON because pulling serde_json in would balloon
    // the wasm binary for a single object shape. The full set of
    // characters EPUB metadata can carry is wide (book descriptions
    // ship UTF-8 freely), so the escaper handles \ " \n \r \t and
    // control chars; everything else passes through literally.

    fn push_json_string(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    // Other control chars get \u00XX form rather than
                    // a lossy replace -- the EPUB metadata is rarely
                    // worth re-encoding.
                    let _ = core::fmt::Write::write_fmt(
                        out,
                        format_args!("\\u{:04x}", c as u32),
                    );
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    fn json_array(items: &[String]) -> String {
        let mut out = String::from("[");
        let mut first = true;
        for s in items {
            if !first { out.push(','); }
            first = false;
            push_json_string(&mut out, s);
        }
        out.push(']');
        out
    }

    /// Build the full epub_metadata JSON object. Keys are stable;
    /// absent fields are omitted (rather than emitted as null) so
    /// downstream `json_extract` calls return SQL NULL on missing
    /// keys, matching the per-field accessor behavior.
    fn metadata_json(m: &EpubMeta) -> String {
        let mut out = String::from("{");
        let mut first = true;
        let comma = |out: &mut String, first: &mut bool| {
            if !*first { out.push(','); }
            *first = false;
        };

        if let Some(v) = &m.title {
            comma(&mut out, &mut first);
            out.push_str("\"title\":");
            push_json_string(&mut out, v);
        }
        if !m.authors.is_empty() {
            comma(&mut out, &mut first);
            out.push_str("\"authors\":");
            out.push_str(&json_array(&m.authors));
        }
        if let Some(v) = &m.language {
            comma(&mut out, &mut first);
            out.push_str("\"language\":");
            push_json_string(&mut out, v);
        }
        if let Some(v) = &m.publisher {
            comma(&mut out, &mut first);
            out.push_str("\"publisher\":");
            push_json_string(&mut out, v);
        }
        if let Some(v) = &m.date {
            comma(&mut out, &mut first);
            out.push_str("\"date\":");
            push_json_string(&mut out, &normalize_date(v));
        }
        if let Some(v) = &m.identifier {
            comma(&mut out, &mut first);
            out.push_str("\"identifier\":");
            push_json_string(&mut out, v);
        }
        if !m.subjects.is_empty() {
            comma(&mut out, &mut first);
            out.push_str("\"subjects\":");
            out.push_str(&json_array(&m.subjects));
        }
        if let Some(v) = &m.description {
            comma(&mut out, &mut first);
            out.push_str("\"description\":");
            push_json_string(&mut out, v);
        }
        if let Some(v) = &m.rights {
            comma(&mut out, &mut first);
            out.push_str("\"rights\":");
            push_json_string(&mut out, v);
        }
        if !m.contributors.is_empty() {
            comma(&mut out, &mut first);
            out.push_str("\"contributors\":");
            out.push_str(&json_array(&m.contributors));
        }
        comma(&mut out, &mut first);
        let _ = core::fmt::Write::write_fmt(
            &mut out,
            format_args!("\"chapter_count\":{}", m.chapter_count),
        );
        if let Some(v) = &m.pkg_version {
            out.push(',');
            out.push_str("\"version\":");
            push_json_string(&mut out, v);
        }
        out.push('}');
        out
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Pure functions of the input blob -- fully deterministic.
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "epub".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: vec![
                    s(FID_TITLE, "epub_title", 1, det),
                    s(FID_AUTHORS, "epub_authors", 1, det),
                    s(FID_LANGUAGE, "epub_language", 1, det),
                    s(FID_PUBLISHER, "epub_publisher", 1, det),
                    s(FID_PUBLISHED_DATE, "epub_published_date", 1, det),
                    s(FID_IDENTIFIER, "epub_identifier", 1, det),
                    s(FID_SUBJECTS, "epub_subjects", 1, det),
                    s(FID_CHAPTER_COUNT, "epub_chapter_count", 1, det),
                    s(FID_METADATA, "epub_metadata", 1, det),
                    s(FID_VERSION, "epub_version", 0, det),
                ],
                aggregate_functions: vec![],
                collations: vec![],
                vtabs: vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            // FID_VERSION is the only zero-arg / blob-free function.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(format!(
                    "epub extension {}; zip 2 + quick-xml 0.36",
                    env!("CARGO_PKG_VERSION")
                )));
            }

            // For all blob-consuming fns, decoding failures collapse
            // to NULL via the same opt_bytes / parse(...) pair.
            let Some(bytes) = opt_bytes(&args) else {
                return Ok(SqlValue::Null);
            };
            let Some(meta) = parse(&bytes) else {
                return Ok(SqlValue::Null);
            };

            match func_id {
                FID_TITLE => match &meta.title {
                    Some(t) => Ok(SqlValue::Text(t.clone())),
                    None => Ok(SqlValue::Null),
                },
                FID_AUTHORS => {
                    // Always return a JSON array (possibly empty) so
                    // callers can json_array_length without a NULL
                    // check; collapse to NULL only when the parse
                    // failed earlier.
                    Ok(SqlValue::Text(json_array(&meta.authors)))
                }
                FID_LANGUAGE => match &meta.language {
                    Some(t) => Ok(SqlValue::Text(t.clone())),
                    None => Ok(SqlValue::Null),
                },
                FID_PUBLISHER => match &meta.publisher {
                    Some(t) => Ok(SqlValue::Text(t.clone())),
                    None => Ok(SqlValue::Null),
                },
                FID_PUBLISHED_DATE => match &meta.date {
                    Some(t) => Ok(SqlValue::Text(normalize_date(t))),
                    None => Ok(SqlValue::Null),
                },
                FID_IDENTIFIER => match &meta.identifier {
                    Some(t) => Ok(SqlValue::Text(t.clone())),
                    None => Ok(SqlValue::Null),
                },
                FID_SUBJECTS => Ok(SqlValue::Text(json_array(&meta.subjects))),
                FID_CHAPTER_COUNT => Ok(SqlValue::Integer(meta.chapter_count as i64)),
                FID_METADATA => Ok(SqlValue::Text(metadata_json(&meta))),
                other => Err(format!("epub: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
