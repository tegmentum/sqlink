//! INI / XML codecs. (TOML codecs moved to extensions/toml.)

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ── INI ────────────────────────────────────────────────────
//
// Tiny hand-rolled INI parser  no separate crate needed. Honors:
//   - [section] headers (subsequent keys nest under this name)
//   - key = value pairs (whitespace trimmed)
//   - ; and # for line comments
//   - global keys before any [section] go under top-level
//
// Doesn't support multi-line values, escape sequences, or
// duplicate-key array semantics  fine for simple config files.

pub fn ini_to_json(text: &str) -> String {
    use serde_json::{Map, Value};
    let mut root: Map<String, Value> = Map::new();
    let mut current: Map<String, Value> = Map::new();
    let mut section_name: Option<String> = None;

    let flush = |section_name: &mut Option<String>,
                 current: &mut Map<String, Value>,
                 root: &mut Map<String, Value>| {
        if let Some(name) = section_name.take() {
            root.insert(name, Value::Object(core::mem::take(current)));
        }
    };

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            // Section transition  flush the previous section.
            // Global (pre-first-section) keys end up under
            // the empty-name key; lift them into root if it
            // exists.
            if section_name.is_none() && !current.is_empty() {
                // Hoist global keys directly into root.
                for (k, v) in core::mem::take(&mut current) {
                    root.insert(k, v);
                }
            } else {
                flush(&mut section_name, &mut current, &mut root);
            }
            section_name = Some(line[1..line.len() - 1].trim().to_string());
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().to_string();
        let v = v.trim().to_string();
        // Try INTEGER / REAL / BOOL coercions for richer JSON.
        let value = if let Ok(n) = v.parse::<i64>() {
            Value::Number(n.into())
        } else if let Ok(r) = v.parse::<f64>() {
            serde_json::Number::from_f64(r)
                .map(Value::Number)
                .unwrap_or(Value::String(v.clone()))
        } else if v.eq_ignore_ascii_case("true") {
            Value::Bool(true)
        } else if v.eq_ignore_ascii_case("false") {
            Value::Bool(false)
        } else {
            // Strip surrounding quotes if present.
            let s = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(&v);
            Value::String(s.to_string())
        };
        current.insert(k, value);
    }
    flush(&mut section_name, &mut current, &mut root);
    // Also flush globals that landed in `current` if there was no section.
    for (k, v) in current {
        root.insert(k, v);
    }
    serde_json::Value::Object(root).to_string()
}

pub fn json_to_ini(text: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| alloc::format!("json_to_ini: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "json_to_ini: top-level must be object".to_string())?;
    let mut out = String::new();
    // First pass: scalar top-level keys.
    for (k, val) in obj {
        if !val.is_object() {
            out.push_str(&alloc::format!("{} = {}\n", k, render_scalar(val)));
        }
    }
    // Second pass: object sections.
    for (k, val) in obj {
        if let Some(section) = val.as_object() {
            out.push('\n');
            out.push_str(&alloc::format!("[{}]\n", k));
            for (kk, vv) in section {
                out.push_str(&alloc::format!("{} = {}\n", kk, render_scalar(vv)));
            }
        }
    }
    Ok(out)
}

fn render_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ── XML  XPath-lite ───────────────────────────────────────
//
// quick-xml ships a streaming parser + tree walker; we wrap a
// dead-simple subset of XPath supporting:
//   `/a/b`           absolute path from root
//   `//tag`           descendant-anywhere
//   `/a/b/@attr`     attribute selector at any level
// No predicates / functions / namespace prefixes. Sufficient
// for the "extract a value from a known-shape XML doc" use
// case; for anything richer the caller can pre-process via
// xml_to_json.

#[derive(Debug, Clone, Default)]
struct XmlElement {
    name: String,
    attrs: alloc::collections::BTreeMap<String, String>,
    text: String,
    children: Vec<XmlElement>,
}

fn parse_xml(doc: &str) -> Result<XmlElement, String> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;
    let mut reader = Reader::from_str(doc);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<XmlElement> = alloc::vec![XmlElement::default()];
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(alloc::format!("xml parse: {e}")),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let mut el = XmlElement::default();
                el.name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                for attr in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
                    let v = String::from_utf8_lossy(&attr.value).into_owned();
                    el.attrs.insert(k, v);
                }
                stack.push(el);
            }
            Ok(Event::Empty(e)) => {
                let mut el = XmlElement::default();
                el.name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                for attr in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
                    let v = String::from_utf8_lossy(&attr.value).into_owned();
                    el.attrs.insert(k, v);
                }
                stack.last_mut().unwrap().children.push(el);
            }
            Ok(Event::Text(t)) => {
                let txt = t.unescape().map(|s| s.into_owned()).unwrap_or_default();
                if let Some(last) = stack.last_mut() {
                    last.text.push_str(&txt);
                }
            }
            Ok(Event::End(_)) => {
                if stack.len() <= 1 {
                    return Err("xml: end without matching start".into());
                }
                let el = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(el);
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(stack.into_iter().next().unwrap())
}

fn xpath_step(
    steps: &[&str],
    el: &XmlElement,
    out: &mut Vec<XmlElement>,
    attr: &mut Option<String>,
) {
    if steps.is_empty() {
        out.push(el.clone());
        return;
    }
    let step = steps[0];
    let rest = &steps[1..];
    // Attribute selector last step: `@name`.
    if step.starts_with('@') {
        if let Some(v) = el.attrs.get(&step[1..]) {
            *attr = Some(v.clone());
        }
        return;
    }
    if step == "*" {
        for c in &el.children {
            xpath_step(rest, c, out, attr);
        }
        return;
    }
    // `**` / descendant-anywhere: match any descendant by name.
    if step.starts_with("**") {
        let name = &step[2..];
        for c in el.descendants() {
            if c.name == *name || name.is_empty() {
                xpath_step(rest, c, out, attr);
            }
        }
        return;
    }
    for c in &el.children {
        if c.name == *step {
            xpath_step(rest, c, out, attr);
        }
    }
}

impl XmlElement {
    fn descendants(&self) -> Vec<&XmlElement> {
        let mut out: Vec<&XmlElement> = Vec::new();
        let mut stack: Vec<&XmlElement> = self.children.iter().collect();
        while let Some(el) = stack.pop() {
            out.push(el);
            for c in &el.children {
                stack.push(c);
            }
        }
        out
    }

    fn text_recursive(&self) -> String {
        let mut s = self.text.clone();
        for c in &self.children {
            s.push_str(&c.text_recursive());
        }
        s
    }
}

fn parse_xpath_lite(expr: &str) -> Vec<&str> {
    // Translate `//tag` to `**tag` for our internal grammar
    // (descendant-anywhere). `/a/b` becomes ["a","b"]. Empty
    // segments from a leading `/` are dropped.
    let normalized = expr.replace("//", "/**");
    normalized
        .strip_prefix('/')
        .unwrap_or(&normalized)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            // Leak so the &strs returned outlive the temp
            // String created by replace(). The lifetime is
            // bounded by parse_xpath_lite's caller.
            Box::leak(s.to_string().into_boxed_str()) as &str
        })
        .collect()
}

pub fn xml_extract(doc: &str, xpath: &str) -> Result<String, String> {
    let root = parse_xml(doc)?;
    let steps = parse_xpath_lite(xpath);
    let mut hits: Vec<XmlElement> = Vec::new();
    let mut attr: Option<String> = None;
    xpath_step(&steps, &root, &mut hits, &mut attr);
    if let Some(a) = attr {
        return Ok(a);
    }
    Ok(hits
        .iter()
        .map(|e| e.text_recursive())
        .collect::<Vec<_>>()
        .join(""))
}

pub fn xml_attr(doc: &str, xpath: &str, attr: &str) -> Result<String, String> {
    let root = parse_xml(doc)?;
    let steps = parse_xpath_lite(xpath);
    let mut hits: Vec<XmlElement> = Vec::new();
    let mut attr_match: Option<String> = None;
    xpath_step(&steps, &root, &mut hits, &mut attr_match);
    if let Some(first) = hits.first() {
        if let Some(v) = first.attrs.get(attr) {
            return Ok(v.clone());
        }
    }
    Ok(String::new())
}

pub fn xml_to_json(doc: &str) -> Result<String, String> {
    let root = parse_xml(doc)?;
    let json = element_to_json(&root);
    Ok(json.to_string())
}

fn element_to_json(el: &XmlElement) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    if !el.attrs.is_empty() {
        let mut attrs = serde_json::Map::new();
        for (k, v) in &el.attrs {
            attrs.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        m.insert("@attrs".into(), serde_json::Value::Object(attrs));
    }
    let trimmed_text = el.text.trim();
    if !trimmed_text.is_empty() {
        m.insert(
            "#text".into(),
            serde_json::Value::String(trimmed_text.to_string()),
        );
    }
    // Group children by name; if a name appears multiple times
    // it becomes an array.
    let mut by_name: alloc::collections::BTreeMap<String, Vec<serde_json::Value>> =
        alloc::collections::BTreeMap::new();
    for c in &el.children {
        by_name
            .entry(c.name.clone())
            .or_default()
            .push(element_to_json(c));
    }
    for (name, vals) in by_name {
        if vals.len() == 1 {
            m.insert(name, vals.into_iter().next().unwrap());
        } else {
            m.insert(name, serde_json::Value::Array(vals));
        }
    }
    serde_json::Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_basic() {
        let ini = "global=42\n\n[server]\nport=8080\nhost=localhost\n";
        let j = ini_to_json(ini);
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["global"], 42);
        assert_eq!(v["server"]["port"], 8080);
        assert_eq!(v["server"]["host"], "localhost");
    }

    #[test]
    fn xml_extract_path() {
        let doc = "<root><user><name>Alice</name></user></root>";
        assert_eq!(xml_extract(doc, "/root/user/name").unwrap(), "Alice");
    }

    #[test]
    fn xml_descendant_anywhere() {
        let doc = "<a><b><c>hit</c></b></a>";
        assert_eq!(xml_extract(doc, "//c").unwrap(), "hit");
    }

    #[test]
    fn xml_attr_lookup() {
        let doc = r#"<root><a href="/x">link</a></root>"#;
        assert_eq!(xml_attr(doc, "/root/a", "href").unwrap(), "/x");
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

    const FID_INI_TO_JSON: u64 = 3;
    const FID_JSON_TO_INI: u64 = 4;
    const FID_XML_EXTRACT: u64 = 5;
    const FID_XML_TO_JSON: u64 = 6;
    const FID_XML_ATTR: u64 = 7;

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
                name: "formats".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_INI_TO_JSON, "ini_to_json", 1),
                    s(FID_JSON_TO_INI, "json_to_ini", 1),
                    s(FID_XML_EXTRACT, "xml_extract", 2),
                    s(FID_XML_TO_JSON, "xml_to_json", 1),
                    s(FID_XML_ATTR, "xml_attr", 3),
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
                preferred_prefix: Some("formats".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.formats".into()),
                typed_values: Vec::new(),
            }
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
                FID_INI_TO_JSON => Ok(SqlValue::Text(super::ini_to_json(&arg_text(
                    &args,
                    0,
                    "ini_to_json",
                )?))),
                FID_JSON_TO_INI => {
                    super::json_to_ini(&arg_text(&args, 0, "json_to_ini")?).map(SqlValue::Text)
                }
                FID_XML_EXTRACT => super::xml_extract(
                    &arg_text(&args, 0, "xml_extract")?,
                    &arg_text(&args, 1, "xml_extract")?,
                )
                .map(SqlValue::Text),
                FID_XML_TO_JSON => {
                    super::xml_to_json(&arg_text(&args, 0, "xml_to_json")?).map(SqlValue::Text)
                }
                FID_XML_ATTR => super::xml_attr(
                    &arg_text(&args, 0, "xml_attr")?,
                    &arg_text(&args, 1, "xml_attr")?,
                    &arg_text(&args, 2, "xml_attr")?,
                )
                .map(SqlValue::Text),
                other => Err(format!("formats: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
