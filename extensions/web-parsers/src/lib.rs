//! JSONPath + HTML/CSS selector helpers.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub fn jsonpath(doc: &str, expr: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(doc).map_err(|e| alloc::format!("jsonpath: JSON: {e}"))?;
    let path = serde_json_path::JsonPath::parse(expr)
        .map_err(|e| alloc::format!("jsonpath: parse {expr:?}: {e}"))?;
    let nodes = path.query(&v);
    let out: Vec<&serde_json::Value> = nodes.all();
    Ok(serde_json::Value::Array(out.into_iter().cloned().collect()).to_string())
}

pub fn jsonpath_first(doc: &str, expr: &str) -> Result<Option<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(doc).map_err(|e| alloc::format!("jsonpath_first: JSON: {e}"))?;
    let path = serde_json_path::JsonPath::parse(expr)
        .map_err(|e| alloc::format!("jsonpath_first: parse {expr:?}: {e}"))?;
    Ok(path.query(&v).first().map(|n| n.to_string()))
}

pub fn jsonpath_exists(doc: &str, expr: &str) -> Result<bool, String> {
    let v: serde_json::Value =
        serde_json::from_str(doc).map_err(|e| alloc::format!("jsonpath_exists: JSON: {e}"))?;
    let path = serde_json_path::JsonPath::parse(expr)
        .map_err(|e| alloc::format!("jsonpath_exists: parse {expr:?}: {e}"))?;
    Ok(!path.query(&v).all().is_empty())
}

pub fn jsonpath_count(doc: &str, expr: &str) -> Result<usize, String> {
    let v: serde_json::Value =
        serde_json::from_str(doc).map_err(|e| alloc::format!("jsonpath_count: JSON: {e}"))?;
    let path = serde_json_path::JsonPath::parse(expr)
        .map_err(|e| alloc::format!("jsonpath_count: parse {expr:?}: {e}"))?;
    Ok(path.query(&v).all().len())
}

pub fn html_extract(doc: &str, selector: &str) -> Result<String, String> {
    use scraper::{Html, Selector};
    let html = Html::parse_document(doc);
    let sel = Selector::parse(selector)
        .map_err(|e| alloc::format!("html_extract: selector {selector:?}: {e:?}"))?;
    let texts: Vec<String> = html
        .select(&sel)
        .map(|el| el.text().collect::<Vec<_>>().join(""))
        .collect();
    Ok(texts.join(""))
}

pub fn html_extract_all(doc: &str, selector: &str) -> Result<String, String> {
    use scraper::{Html, Selector};
    let html = Html::parse_document(doc);
    let sel = Selector::parse(selector)
        .map_err(|e| alloc::format!("html_extract_all: selector {selector:?}: {e:?}"))?;
    let items: Vec<serde_json::Value> = html
        .select(&sel)
        .map(|el| serde_json::Value::String(el.text().collect::<Vec<_>>().join("")))
        .collect();
    Ok(serde_json::Value::Array(items).to_string())
}

pub fn html_attr(doc: &str, selector: &str, attr: &str) -> Result<Option<String>, String> {
    use scraper::{Html, Selector};
    let html = Html::parse_document(doc);
    let sel = Selector::parse(selector)
        .map_err(|e| alloc::format!("html_attr: selector {selector:?}: {e:?}"))?;
    Ok(html.select(&sel).next().and_then(|el| el.value().attr(attr).map(|s| s.to_string())))
}

pub fn html_text(doc: &str) -> Result<String, String> {
    use scraper::{Html, Selector};
    let html = Html::parse_document(doc);
    let body = Selector::parse("body").map_err(|e| alloc::format!("html_text: {e:?}"))?;
    Ok(html
        .select(&body)
        .next()
        .map(|el| el.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{"users":[{"name":"alice","age":30},{"name":"bob","age":25}]}"#;

    #[test]
    fn jsonpath_returns_matches() {
        let r = jsonpath(DOC, "$.users[*].name").unwrap();
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn jsonpath_first_returns_one() {
        let r = jsonpath_first(DOC, "$.users[0].name").unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v.as_str().unwrap(), "alice");
    }

    #[test]
    fn jsonpath_exists_yes_and_no() {
        assert!(jsonpath_exists(DOC, "$.users[0]").unwrap());
        assert!(!jsonpath_exists(DOC, "$.nothing").unwrap());
    }

    const HTML: &str = r#"
<html><body>
    <h1 id="title">Welcome</h1>
    <ul class="links">
        <li><a href="/a">first</a></li>
        <li><a href="/b">second</a></li>
    </ul>
</body></html>
"#;

    #[test]
    fn html_extract_concatenates_text() {
        let r = html_extract(HTML, "h1").unwrap();
        assert!(r.contains("Welcome"));
    }

    #[test]
    fn html_extract_all_returns_array() {
        let r = html_extract_all(HTML, "ul.links li a").unwrap();
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str().unwrap(), "first");
        assert_eq!(arr[1].as_str().unwrap(), "second");
    }

    #[test]
    fn html_attr_finds_href() {
        let r = html_attr(HTML, "ul.links li a", "href").unwrap();
        assert_eq!(r.unwrap(), "/a");
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

    const FID_JP: u64 = 1;
    const FID_JP_FIRST: u64 = 2;
    const FID_JP_EXISTS: u64 = 3;
    const FID_H_EXTRACT: u64 = 4;
    const FID_H_EXTRACT_ALL: u64 = 5;
    const FID_H_ATTR: u64 = 6;
    const FID_H_TEXT: u64 = 7;
    const FID_JP_COUNT: u64 = 8;

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
                name: "web-parsers".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_JP, "jsonpath", 2),
                    s(FID_JP_FIRST, "jsonpath_first", 2),
                    s(FID_JP_EXISTS, "jsonpath_exists", 2),
                    s(FID_JP_COUNT, "jsonpath_count", 2),
                    s(FID_H_EXTRACT, "html_extract", 2),
                    s(FID_H_EXTRACT_ALL, "html_extract_all", 2),
                    s(FID_H_ATTR, "html_attr", 3),
                    s(FID_H_TEXT, "html_text", 1),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
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
                FID_JP => {
                    let d = arg_text(&args, 0, "jsonpath")?;
                    let e = arg_text(&args, 1, "jsonpath")?;
                    super::jsonpath(&d, &e).map(SqlValue::Text)
                }
                FID_JP_FIRST => {
                    let d = arg_text(&args, 0, "jsonpath_first")?;
                    let e = arg_text(&args, 1, "jsonpath_first")?;
                    super::jsonpath_first(&d, &e)
                        .map(|o| o.map(SqlValue::Text).unwrap_or(SqlValue::Null))
                }
                FID_JP_EXISTS => {
                    let d = arg_text(&args, 0, "jsonpath_exists")?;
                    let e = arg_text(&args, 1, "jsonpath_exists")?;
                    super::jsonpath_exists(&d, &e).map(|b| SqlValue::Integer(b as i64))
                }
                FID_JP_COUNT => {
                    let d = arg_text(&args, 0, "jsonpath_count")?;
                    let e = arg_text(&args, 1, "jsonpath_count")?;
                    super::jsonpath_count(&d, &e).map(|n| SqlValue::Integer(n as i64))
                }
                FID_H_EXTRACT => {
                    let d = arg_text(&args, 0, "html_extract")?;
                    let s = arg_text(&args, 1, "html_extract")?;
                    super::html_extract(&d, &s).map(SqlValue::Text)
                }
                FID_H_EXTRACT_ALL => {
                    let d = arg_text(&args, 0, "html_extract_all")?;
                    let s = arg_text(&args, 1, "html_extract_all")?;
                    super::html_extract_all(&d, &s).map(SqlValue::Text)
                }
                FID_H_ATTR => {
                    let d = arg_text(&args, 0, "html_attr")?;
                    let s = arg_text(&args, 1, "html_attr")?;
                    let a = arg_text(&args, 2, "html_attr")?;
                    super::html_attr(&d, &s, &a)
                        .map(|o| o.map(SqlValue::Text).unwrap_or(SqlValue::Null))
                }
                FID_H_TEXT => {
                    let d = arg_text(&args, 0, "html_text")?;
                    super::html_text(&d).map(SqlValue::Text)
                }
                other => Err(format!("web-parsers: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
