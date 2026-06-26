//! HTML extension for SQLite.
//!
//! Function surface (PLAN-more-extensions-3.md #3):
//!
//!   html_to_text(html)                  -> text  (strip tags + decode entities)
//!   html_get_text(html, selector)       -> text  (CSS selector, first match's text)
//!   html_get_attr(html, selector, attr) -> text  (first match's attribute value)
//!   html_all_text(html, selector)       -> text  (JSON array of all matches' text)
//!   html_decode_entities(s)             -> text
//!   html_encode_entities(s)             -> text
//!   html_strip_tags(s)                  -> text  (tag-strip only, no entity decode)
//!   html_links(html)                    -> text  (JSON array of href values)
//!   html_images(html)                   -> text  (JSON array of {src, alt})
//!   html_title(html)                    -> text  (first <title> contents)
//!   html_version()                      -> text
//!
//! NULL -> NULL on every fn that takes HTML / TEXT input. Malformed
//! HTML still parses -- html5ever is intentionally liberal.

extern crate alloc;

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ───────── core helpers (target-independent) ─────────

/// Strip tags via a small state machine. Used by both
/// `html_strip_tags` (no entity decode) and as the fast path for
/// `html_to_text` (which then runs the result through the entity
/// decoder). Mirrors the behavior of a single-pass tag remover:
/// anything between `<` and `>` is dropped. HTML comments
/// (`<!-- ... -->`) and CDATA-ish sequences are handled by the
/// same drop -- inside `<>` is text we don't keep.
pub fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

pub fn decode_entities(s: &str) -> String {
    html_escape::decode_html_entities(s).into_owned()
}

pub fn encode_entities(s: &str) -> String {
    // Encode the "safe" set: `<`, `>`, `&`, `"`, `'`. This is what
    // every common templating library does by default and matches
    // the spec's "safe HTML" set for attribute + text contexts.
    html_escape::encode_safe(s).into_owned()
}

/// Parse + extract concatenated text via scraper's tree walker.
/// Used by `html_to_text` (whole document) and `html_get_text`
/// (single match for a CSS selector).
fn document_text(html: &str) -> String {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    // Prefer <body> when present; otherwise walk the root.
    let body_sel = Selector::parse("body").expect("body selector");
    let html_sel = Selector::parse("html").expect("html selector");
    if let Some(el) = doc.select(&body_sel).next() {
        return el.text().collect::<Vec<_>>().join("");
    }
    if let Some(el) = doc.select(&html_sel).next() {
        return el.text().collect::<Vec<_>>().join("");
    }
    String::new()
}

pub fn html_to_text(html: &str) -> String {
    document_text(html)
}

pub fn html_get_text(html: &str, selector: &str) -> Result<Option<String>, String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let sel = Selector::parse(selector)
        .map_err(|e| alloc::format!("html_get_text: selector {selector:?}: {e:?}"))?;
    Ok(doc
        .select(&sel)
        .next()
        .map(|el| el.text().collect::<Vec<_>>().join("")))
}

pub fn html_get_attr(
    html: &str,
    selector: &str,
    attr: &str,
) -> Result<Option<String>, String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let sel = Selector::parse(selector)
        .map_err(|e| alloc::format!("html_get_attr: selector {selector:?}: {e:?}"))?;
    Ok(doc
        .select(&sel)
        .next()
        .and_then(|el| el.value().attr(attr).map(|s| s.to_owned())))
}

pub fn html_all_text(html: &str, selector: &str) -> Result<String, String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let sel = Selector::parse(selector)
        .map_err(|e| alloc::format!("html_all_text: selector {selector:?}: {e:?}"))?;
    let items: Vec<serde_json::Value> = doc
        .select(&sel)
        .map(|el| serde_json::Value::String(el.text().collect::<Vec<_>>().join("")))
        .collect();
    Ok(serde_json::Value::Array(items).to_string())
}

pub fn html_links(html: &str) -> String {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    // a[href] picks anchors with an actual href. Anchors without
    // href are uncommon but valid; skipping them keeps the JSON
    // array clean of nulls.
    let sel = Selector::parse("a[href]").expect("a[href] selector");
    let items: Vec<serde_json::Value> = doc
        .select(&sel)
        .filter_map(|el| el.value().attr("href").map(|s| serde_json::Value::String(s.to_owned())))
        .collect();
    serde_json::Value::Array(items).to_string()
}

pub fn html_images(html: &str) -> String {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let sel = Selector::parse("img").expect("img selector");
    let items: Vec<serde_json::Value> = doc
        .select(&sel)
        .map(|el| {
            // `src` is the only field we guarantee; `alt` defaults
            // to "" when missing (matches the common convention
            // that absent alt == empty alt for accessibility).
            let src = el.value().attr("src").unwrap_or("");
            let alt = el.value().attr("alt").unwrap_or("");
            let mut obj = serde_json::Map::new();
            obj.insert("src".to_string(), serde_json::Value::String(src.to_owned()));
            obj.insert("alt".to_string(), serde_json::Value::String(alt.to_owned()));
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::Value::Array(items).to_string()
}

pub fn html_title(html: &str) -> Option<String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let sel = Selector::parse("title").expect("title selector");
    doc.select(&sel)
        .next()
        .map(|el| el.text().collect::<Vec<_>>().join(""))
}

// ───────── tests (host-side; not built for wasm) ─────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_tags_drops_markup() {
        assert_eq!(strip_tags("<p>hi</p>"), "hi");
        assert_eq!(strip_tags("a<b>c</b>d"), "acd");
    }

    #[test]
    fn entity_round_trip() {
        assert_eq!(decode_entities("&lt;b&gt;hi&amp;"), "<b>hi&");
        assert_eq!(encode_entities("<b>"), "&lt;b&gt;");
    }

    #[test]
    fn body_text_extraction() {
        assert_eq!(html_to_text("<p>hi</p>"), "hi");
    }

    #[test]
    fn selector_text_match() {
        assert_eq!(
            html_get_text("<p class=\"x\">a</p><p>b</p>", ".x").unwrap(),
            Some("a".to_string())
        );
    }

    #[test]
    fn attr_extraction() {
        assert_eq!(
            html_get_attr("<a href=\"/x\">L</a>", "a", "href").unwrap(),
            Some("/x".to_string())
        );
    }

    #[test]
    fn links_array() {
        let r = html_links("<a href=\"/a\">L</a><a href=\"/b\">M</a>");
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn title_extraction() {
        assert_eq!(
            html_title("<html><head><title>T</title></head></html>"),
            Some("T".to_string())
        );
    }
}

// ───────── wasm export shim ─────────

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

    const FID_TO_TEXT: u64 = 1;
    const FID_GET_TEXT: u64 = 2;
    const FID_GET_ATTR: u64 = 3;
    const FID_ALL_TEXT: u64 = 4;
    const FID_DECODE: u64 = 5;
    const FID_ENCODE: u64 = 6;
    const FID_STRIP: u64 = 7;
    const FID_LINKS: u64 = 8;
    const FID_IMAGES: u64 = 9;
    const FID_TITLE: u64 = 10;
    const FID_VERSION: u64 = 11;

    struct Ext;

    /// Pull a TEXT argument. NULL propagates as `Ok(None)` so the
    /// caller can short-circuit to `SqlValue::Null`. Any non-TEXT,
    /// non-NULL value is an error -- HTML is text by definition.
    fn arg_text_or_null(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(SqlValue::Null) => Ok(None),
            None => Err(format!("{fname}: missing arg at index {i}")),
            Some(_) => Err(format!("{fname}: arg at {i} must be TEXT")),
            // PLAN-wit-value-extension.md Phase A: the sql-value variant
            // gained a wit-value arm; Phase B will replace this wildcard
            // with extension-specific decode/encode logic.
            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
        }
    }

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
                name: "html".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TO_TEXT, "html_to_text", 1),
                    s(FID_GET_TEXT, "html_get_text", 2),
                    s(FID_GET_ATTR, "html_get_attr", 3),
                    s(FID_ALL_TEXT, "html_all_text", 2),
                    s(FID_DECODE, "html_decode_entities", 1),
                    s(FID_ENCODE, "html_encode_entities", 1),
                    s(FID_STRIP, "html_strip_tags", 1),
                    s(FID_LINKS, "html_links", 1),
                    s(FID_IMAGES, "html_images", 1),
                    s(FID_TITLE, "html_title", 1),
                    s(FID_VERSION, "html_version", 0),
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
                preferred_prefix: Some("html".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.html".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_TO_TEXT => {
                    let Some(h) = arg_text_or_null(&args, 0, "html_to_text")? else {
                        return Ok(SqlValue::Null);
                    };
                    // html_to_text = strip tags via the parsed
                    // document (this preserves text order across
                    // mixed-content nodes) + decode entities.
                    let raw = super::html_to_text(&h);
                    Ok(SqlValue::Text(super::decode_entities(&raw)))
                }
                FID_GET_TEXT => {
                    let Some(h) = arg_text_or_null(&args, 0, "html_get_text")? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(sel) = arg_text_or_null(&args, 1, "html_get_text")? else {
                        return Ok(SqlValue::Null);
                    };
                    match super::html_get_text(&h, &sel)? {
                        Some(t) => Ok(SqlValue::Text(t)),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_GET_ATTR => {
                    let Some(h) = arg_text_or_null(&args, 0, "html_get_attr")? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(sel) = arg_text_or_null(&args, 1, "html_get_attr")? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(attr) = arg_text_or_null(&args, 2, "html_get_attr")? else {
                        return Ok(SqlValue::Null);
                    };
                    match super::html_get_attr(&h, &sel, &attr)? {
                        Some(t) => Ok(SqlValue::Text(t)),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_ALL_TEXT => {
                    let Some(h) = arg_text_or_null(&args, 0, "html_all_text")? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(sel) = arg_text_or_null(&args, 1, "html_all_text")? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Text(super::html_all_text(&h, &sel)?))
                }
                FID_DECODE => {
                    let Some(s) = arg_text_or_null(&args, 0, "html_decode_entities")? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Text(super::decode_entities(&s)))
                }
                FID_ENCODE => {
                    let Some(s) = arg_text_or_null(&args, 0, "html_encode_entities")? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Text(super::encode_entities(&s)))
                }
                FID_STRIP => {
                    let Some(s) = arg_text_or_null(&args, 0, "html_strip_tags")? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Text(super::strip_tags(&s)))
                }
                FID_LINKS => {
                    let Some(h) = arg_text_or_null(&args, 0, "html_links")? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Text(super::html_links(&h)))
                }
                FID_IMAGES => {
                    let Some(h) = arg_text_or_null(&args, 0, "html_images")? else {
                        return Ok(SqlValue::Null);
                    };
                    Ok(SqlValue::Text(super::html_images(&h)))
                }
                FID_TITLE => {
                    let Some(h) = arg_text_or_null(&args, 0, "html_title")? else {
                        return Ok(SqlValue::Null);
                    };
                    match super::html_title(&h) {
                        Some(t) => Ok(SqlValue::Text(t)),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "html extension {}; scraper 0.21 + html-escape 0.2",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("html: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
