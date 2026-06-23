//! sitemap.xml + sitemap-index parsers for SQLite.
//!
//! Implements the sitemaps.org protocol
//! (https://www.sitemaps.org/protocol.html), which defines two
//! document shapes:
//!
//!   urlset:        <urlset><url><loc>...</loc>...</url>...</urlset>
//!   sitemapindex:  <sitemapindex><sitemap><loc>...</loc>...</sitemap>...
//!
//! Optional per-URL fields (sitemaps.org section "XML tag definitions"):
//!   <loc>          required URL
//!   <lastmod>      W3C-Datetime; surfaced verbatim
//!   <changefreq>   one of always|hourly|daily|weekly|monthly|yearly|never
//!   <priority>     0.0 - 1.0
//!
//! We accept either shape and route the URLs accordingly. The parser
//! is namespace-agnostic: `<urlset xmlns="...">` and `<sm:urlset
//! xmlns:sm="...">` both work because we match on the element's
//! local name (after the colon, if any). Whitespace, comments, CDATA,
//! XML declarations, and BOM markers are tolerated.
//!
//! Function surface (see Cargo.toml for the prose):
//!   sitemap_urls(xml)        -> TEXT
//!   sitemap_full(xml)        -> TEXT
//!   sitemap_index_locs(xml)  -> TEXT
//!   sitemap_count(xml)       -> INTEGER
//!   sitemap_is_valid(xml)    -> INTEGER
//!   sitemap_version()        -> TEXT

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// Parsed shape of a single <url> or <sitemap> record. The four
// optional fields default to None and stay None for sitemap-index
// records (which never have lastmod/changefreq/priority).
#[derive(Default, Clone, Debug, PartialEq)]
pub struct SitemapEntry {
    pub loc: String,
    pub lastmod: Option<String>,
    pub changefreq: Option<String>,
    pub priority: Option<String>,
}

/// Discriminator for the two document shapes the sitemaps.org
/// protocol defines. `Unknown` is reserved for documents whose root
/// is neither `urlset` nor `sitemapindex` (or for parse failures).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SitemapKind {
    UrlSet,
    SitemapIndex,
    Unknown,
}

/// Result of a parse: the discriminated kind + the list of entries.
#[derive(Default, Clone, Debug)]
pub struct Sitemap {
    pub kind: Option<SitemapKind>,
    pub entries: Vec<SitemapEntry>,
}

/// Strip an XML element's namespace prefix. quick-xml's `name()`
/// returns the full QName ("sm:loc") -- we want the local part
/// ("loc") so that namespace-prefixed sitemaps still match.
fn local_name(qname: &[u8]) -> &[u8] {
    match qname.iter().position(|&b| b == b':') {
        Some(i) => &qname[i + 1..],
        None => qname,
    }
}

/// Lossy UTF-8 conversion. Sitemap XML is utf-8 per the protocol,
/// but we never want a stray byte to abort the whole parse -- a
/// downstream caller would rather see a slightly-mangled `<loc>` than
/// no result at all.
fn to_string(b: &[u8]) -> String {
    alloc::string::String::from_utf8_lossy(b).into_owned()
}

/// Single-pass pull-parser. Tracks a tiny state machine: which
/// container we're inside (`urlset` or `sitemapindex`), whether we're
/// inside an `<url>` / `<sitemap>` record, and which optional field
/// we're currently capturing text into. The parser is forgiving:
/// repeated tags within one record keep the *last* value (matches
/// browser-ish parsing), and missing closing tags abort the parse
/// (the SQL-level validity probe surfaces that as 0).
pub fn parse_sitemap(xml: &str) -> Result<Sitemap, String> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    // Keep CDATA intact when callers wrap a URL in `<![CDATA[...]]>`.
    // quick-xml emits these as separate events; we handle them like
    // text below.

    let mut out = Sitemap::default();
    // Outer container (urlset / sitemapindex). Set on first matching
    // <Start>; everything before that is the XML decl, BOM, etc.
    let mut container_seen: Option<SitemapKind> = None;
    // Whether we're inside a per-record element (<url> or <sitemap>).
    let mut in_record = false;
    let mut current: SitemapEntry = SitemapEntry::default();
    // Which child of the record element we're currently inside. None
    // when between fields. Only `loc`, `lastmod`, `changefreq`,
    // `priority` are recognized -- the protocol's optional `image:`
    // and `news:` extensions are ignored.
    let mut current_field: Option<&'static str> = None;
    // Text accumulator for the active field -- quick-xml can split a
    // single text node into multiple events if entities or CDATA are
    // interleaved, so we always append.
    let mut text_buf = String::new();

    let mut buf = Vec::new();

    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| alloc::format!("xml parse: {e}"))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                let name = local_name(e.name().as_ref()).to_vec();
                match name.as_slice() {
                    b"urlset" => container_seen = Some(SitemapKind::UrlSet),
                    b"sitemapindex" => container_seen = Some(SitemapKind::SitemapIndex),
                    b"url" if container_seen == Some(SitemapKind::UrlSet) => {
                        in_record = true;
                        current = SitemapEntry::default();
                    }
                    b"sitemap" if container_seen == Some(SitemapKind::SitemapIndex) => {
                        in_record = true;
                        current = SitemapEntry::default();
                    }
                    b"loc" if in_record => {
                        current_field = Some("loc");
                        text_buf.clear();
                    }
                    b"lastmod" if in_record => {
                        current_field = Some("lastmod");
                        text_buf.clear();
                    }
                    b"changefreq" if in_record => {
                        current_field = Some("changefreq");
                        text_buf.clear();
                    }
                    b"priority" if in_record => {
                        current_field = Some("priority");
                        text_buf.clear();
                    }
                    _ => {
                        // Unknown element -- e.g. an extension
                        // namespace (image:image, news:news). Ignore
                        // its contents by leaving `current_field`
                        // alone; the matching Event::End below will
                        // restore state.
                    }
                }
            }
            Event::Empty(e) => {
                // Self-closing element -- e.g. <loc/> with no body.
                // We don't write anything to the entry, which leaves
                // the field as its default (empty / None).
                let _ = e;
            }
            Event::Text(t) => {
                if current_field.is_some() {
                    let s = t
                        .unescape()
                        .map(|s| s.into_owned())
                        .unwrap_or_else(|_| to_string(t.as_ref()));
                    text_buf.push_str(&s);
                }
            }
            Event::CData(t) => {
                if current_field.is_some() {
                    text_buf.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Event::End(e) => {
                let name = local_name(e.name().as_ref()).to_vec();
                match name.as_slice() {
                    b"url" | b"sitemap" if in_record => {
                        // Sitemap-index records only have <loc>; drop
                        // entries that never set one rather than
                        // emitting blank URLs.
                        if !current.loc.is_empty() {
                            out.entries.push(core::mem::take(&mut current));
                        }
                        in_record = false;
                        current_field = None;
                    }
                    b"loc" if current_field == Some("loc") => {
                        current.loc = text_buf.trim().into();
                        current_field = None;
                    }
                    b"lastmod" if current_field == Some("lastmod") => {
                        let v = text_buf.trim();
                        if !v.is_empty() {
                            current.lastmod = Some(v.into());
                        }
                        current_field = None;
                    }
                    b"changefreq" if current_field == Some("changefreq") => {
                        let v = text_buf.trim();
                        if !v.is_empty() {
                            current.changefreq = Some(v.into());
                        }
                        current_field = None;
                    }
                    b"priority" if current_field == Some("priority") => {
                        let v = text_buf.trim();
                        if !v.is_empty() {
                            current.priority = Some(v.into());
                        }
                        current_field = None;
                    }
                    _ => {
                        // Closing tag for an element we were ignoring
                        // (extension namespace, root). Nothing to do.
                    }
                }
            }
            _ => {
                // Comment, Decl, PI, DocType -- all sitemap-irrelevant.
            }
        }
        buf.clear();
    }

    out.kind = container_seen;
    Ok(out)
}

// ─────────── JSON encoder (no serde_json: keep deps minimal) ───────────

/// Escape one string into a JSON string-literal (double-quoted, the
/// seven mandatory escapes, control chars as \u00XX). The sitemap
/// protocol guarantees utf-8, so embedded non-ASCII is left intact.
fn json_escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&alloc::format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn locs_json(entries: &[SitemapEntry]) -> String {
    let mut s = String::from("[");
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        json_escape(&e.loc, &mut s);
    }
    s.push(']');
    s
}

/// Emit the full record shape:
///   [{"loc":..., "lastmod":..., "changefreq":..., "priority":...}, ...]
/// Optional fields render as null when absent, which keeps the array
/// schema rectangular for downstream json1 callers.
fn full_json(entries: &[SitemapEntry]) -> String {
    let mut s = String::from("[");
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"loc\":");
        json_escape(&e.loc, &mut s);
        s.push_str(",\"lastmod\":");
        match &e.lastmod {
            Some(v) => json_escape(v, &mut s),
            None => s.push_str("null"),
        }
        s.push_str(",\"changefreq\":");
        match &e.changefreq {
            Some(v) => json_escape(v, &mut s),
            None => s.push_str("null"),
        }
        s.push_str(",\"priority\":");
        match &e.priority {
            Some(v) => json_escape(v, &mut s),
            None => s.push_str("null"),
        }
        s.push('}');
    }
    s.push(']');
    s
}

// ─────────── public surface used by the wasm guest impl ───────────

/// Parse + return the JSON array of `<loc>` strings for a `urlset`
/// document. Sitemap-index documents return `[]` here -- callers
/// pick `sitemap_index_locs` for those.
pub fn sitemap_urls(xml: &str) -> Result<String, String> {
    let s = parse_sitemap(xml)?;
    if s.kind == Some(SitemapKind::UrlSet) {
        Ok(locs_json(&s.entries))
    } else {
        Ok(String::from("[]"))
    }
}

/// Full per-URL detail for a `urlset` document.
pub fn sitemap_full(xml: &str) -> Result<String, String> {
    let s = parse_sitemap(xml)?;
    if s.kind == Some(SitemapKind::UrlSet) {
        Ok(full_json(&s.entries))
    } else {
        Ok(String::from("[]"))
    }
}

/// Index `<loc>`s for a `sitemapindex` document. urlset documents
/// return `[]`.
pub fn sitemap_index_locs(xml: &str) -> Result<String, String> {
    let s = parse_sitemap(xml)?;
    if s.kind == Some(SitemapKind::SitemapIndex) {
        Ok(locs_json(&s.entries))
    } else {
        Ok(String::from("[]"))
    }
}

/// Count the records in either shape.
pub fn sitemap_count(xml: &str) -> Result<i64, String> {
    let s = parse_sitemap(xml)?;
    Ok(s.entries.len() as i64)
}

/// 1 if the document parses cleanly *and* the root is one of the
/// two protocol shapes; 0 otherwise. This intentionally rejects
/// `<rss>` / `<feed>` / anything else even when the XML is well-
/// formed, since sitemap consumers want a hard "is this actually
/// a sitemap" gate.
pub fn sitemap_is_valid(xml: &str) -> bool {
    match parse_sitemap(xml) {
        Ok(s) => matches!(s.kind, Some(SitemapKind::UrlSet) | Some(SitemapKind::SitemapIndex)),
        Err(_) => false,
    }
}

// ─────────── tests (native build) ───────────

#[cfg(test)]
mod tests {
    use super::*;

    const URLSET_SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url>
    <loc>https://example.com/a</loc>
    <lastmod>2024-01-15</lastmod>
    <changefreq>weekly</changefreq>
    <priority>0.8</priority>
  </url>
  <url>
    <loc>https://example.com/b</loc>
  </url>
</urlset>
"#;

    const INDEX_SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap>
    <loc>https://example.com/sitemap-1.xml</loc>
    <lastmod>2024-01-01</lastmod>
  </sitemap>
  <sitemap>
    <loc>https://example.com/sitemap-2.xml</loc>
  </sitemap>
</sitemapindex>
"#;

    #[test]
    fn urlset_locs() {
        let got = sitemap_urls(URLSET_SAMPLE).unwrap();
        assert_eq!(got, r#"["https://example.com/a","https://example.com/b"]"#);
    }

    #[test]
    fn urlset_full() {
        let got = sitemap_full(URLSET_SAMPLE).unwrap();
        assert!(got.contains(r#""loc":"https://example.com/a""#));
        assert!(got.contains(r#""lastmod":"2024-01-15""#));
        assert!(got.contains(r#""priority":"0.8""#));
        // Second entry has all-null optional fields.
        assert!(got.contains(r#""loc":"https://example.com/b","lastmod":null,"changefreq":null,"priority":null"#));
    }

    #[test]
    fn index_locs() {
        let got = sitemap_index_locs(INDEX_SAMPLE).unwrap();
        assert_eq!(
            got,
            r#"["https://example.com/sitemap-1.xml","https://example.com/sitemap-2.xml"]"#
        );
    }

    #[test]
    fn counts_correctly() {
        assert_eq!(sitemap_count(URLSET_SAMPLE).unwrap(), 2);
        assert_eq!(sitemap_count(INDEX_SAMPLE).unwrap(), 2);
    }

    #[test]
    fn cross_kind_returns_empty() {
        // urlset asked for index locs -> []
        assert_eq!(sitemap_index_locs(URLSET_SAMPLE).unwrap(), "[]");
        // sitemap-index asked for urlset urls -> []
        assert_eq!(sitemap_urls(INDEX_SAMPLE).unwrap(), "[]");
    }

    #[test]
    fn validity() {
        assert!(sitemap_is_valid(URLSET_SAMPLE));
        assert!(sitemap_is_valid(INDEX_SAMPLE));
        // Not a sitemap shape:
        assert!(!sitemap_is_valid("<rss><channel/></rss>"));
        // Junk:
        assert!(!sitemap_is_valid("not xml at all <<<<"));
        // Empty:
        assert!(!sitemap_is_valid(""));
    }

    #[test]
    fn namespace_prefix_tolerated() {
        let xml = r#"<sm:urlset xmlns:sm="http://www.sitemaps.org/schemas/sitemap/0.9">
            <sm:url><sm:loc>https://example.com/x</sm:loc></sm:url>
        </sm:urlset>"#;
        assert!(sitemap_is_valid(xml));
        assert_eq!(
            sitemap_urls(xml).unwrap(),
            r#"["https://example.com/x"]"#
        );
    }

    #[test]
    fn cdata_loc() {
        let xml = r#"<urlset>
            <url><loc><![CDATA[https://example.com/with&amp;]]></loc></url>
        </urlset>"#;
        assert_eq!(
            sitemap_urls(xml).unwrap(),
            r#"["https://example.com/with&amp;"]"#
        );
    }
}

// ─────────── wasm component export ───────────

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

    // Stable function IDs the host dispatches against. Keep these
    // stable across releases -- consumers index off them when
    // wiring up the manifest.
    const FID_URLS: u64 = 1;
    const FID_FULL: u64 = 2;
    const FID_INDEX_LOCS: u64 = 3;
    const FID_COUNT: u64 = 4;
    const FID_IS_VALID: u64 = 5;
    const FID_VERSION: u64 = 6;

    struct Ext;

    /// Coerce TEXT / BLOB into a String for the parsers. NULL and
    /// numeric values return None so the caller's NULL gate runs.
    /// BLOB is utf-8-lossy decoded -- sitemap XML is utf-8 per spec
    /// but the protocol doesn't promise the body never has stray
    /// bytes, and a strict decode would fail noisily on those.
    fn opt_str(v: &SqlValue) -> Option<String> {
        match v {
            SqlValue::Text(s) => Some(s.clone()),
            SqlValue::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
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
                name: "sitemap-xml".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_URLS, "sitemap_urls", 1, det),
                    s(FID_FULL, "sitemap_full", 1, det),
                    s(FID_INDEX_LOCS, "sitemap_index_locs", 1, det),
                    s(FID_COUNT, "sitemap_count", 1, det),
                    s(FID_IS_VALID, "sitemap_is_valid", 1, det),
                    s(FID_VERSION, "sitemap_version", 0, det),
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
            if func_id == FID_VERSION {
                let v = format!(
                    "sitemap-xml {} (sitemaps.org protocol 0.9; quick-xml 0.36)",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(SqlValue::Text(v));
            }

            // All other functions take exactly one arg.
            let first = match args.first() {
                Some(v) => v,
                None => return Err("sitemap-xml: missing arg".into()),
            };

            // NULL gate. sitemap_is_valid returns 0 for NULL (NULL is
            // not a valid sitemap); the rest return NULL so callers
            // see "no input" distinctly from "empty result".
            if matches!(first, SqlValue::Null) {
                return Ok(match func_id {
                    FID_IS_VALID => SqlValue::Integer(0),
                    _ => SqlValue::Null,
                });
            }

            let body = match opt_str(first) {
                Some(s) => s,
                None => {
                    // Wrong type (INTEGER / REAL) -- treat as NULL
                    // for consistency with the NULL gate.
                    return Ok(match func_id {
                        FID_IS_VALID => SqlValue::Integer(0),
                        _ => SqlValue::Null,
                    });
                }
            };

            match func_id {
                FID_URLS => super::sitemap_urls(&body).map(SqlValue::Text),
                FID_FULL => super::sitemap_full(&body).map(SqlValue::Text),
                FID_INDEX_LOCS => super::sitemap_index_locs(&body).map(SqlValue::Text),
                FID_COUNT => super::sitemap_count(&body).map(SqlValue::Integer),
                FID_IS_VALID => Ok(SqlValue::Integer(super::sitemap_is_valid(&body) as i64)),
                other => Err(format!("sitemap-xml: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
