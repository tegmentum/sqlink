//! vCard (RFC 6350 / RFC 2426) parsing scalars.
//!
//! The `vcard4` crate covers vCard 4.0 (RFC 6350) and 3.0
//! (RFC 2426) cleanly via the `parse_loose` entrypoint. vCard 2.1
//! has so many vendor encoding/folding quirks that we accept best-
//! effort: if parse_loose returns Err on a 2.1 input, every scalar
//! returns NULL — same as on a non-vCard string. This matches the
//! parent plan's "2.1 best-effort" stance.
//!
//! The `vcard4` Vcard struct does not preserve the VERSION header,
//! so `vcard_version_in` greps the raw input directly. The other
//! scalars work against the parsed struct.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};

use vcard4::property::{
    AddressProperty, DateAndOrTime, DateTimeOrTextProperty, TextOrUriProperty, TextProperty,
    UriProperty,
};
use vcard4::{parse_loose, Vcard};

// ─────────────── input helpers ───────────────

/// Try to extract the VERSION line from the raw vCard text. Folded
/// continuation lines are not relevant here — VERSION is a single
/// short token per RFC 6350 § 3.4. Returns the trimmed value, e.g.
/// "3.0" or "4.0".
pub fn version_in(text: &str) -> Option<String> {
    // Case-insensitive scan; CRLF and LF both fine.
    for line in text.lines() {
        let trim = line.trim_start();
        // Match either "VERSION:" or "VERSION:" with leading group/param.
        // Most cards have "VERSION:" on its own line.
        let lower_start = trim
            .get(..8)
            .map(|s| s.eq_ignore_ascii_case("VERSION:"))
            .unwrap_or(false);
        if lower_start {
            let v = trim[8..].trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Parse the input as one (or more) vCards, returning the first.
/// vCard 2.1 inputs frequently fail vcard4's strict tokenizer — we
/// just yield None and let callers return NULL, by design.
pub fn parse_first(text: &str) -> Option<Vcard> {
    parse_loose(text).ok().and_then(|mut v| {
        if v.is_empty() {
            None
        } else {
            Some(v.swap_remove(0))
        }
    })
}

// ─────────────── property extractors ───────────────

pub fn fn_value(c: &Vcard) -> Option<String> {
    c.formatted_name.first().map(|p| p.value.clone())
}

pub fn note_value(c: &Vcard) -> Option<String> {
    c.note.first().map(|p| p.value.clone())
}

pub fn title_value(c: &Vcard) -> Option<String> {
    c.title.first().map(|p| p.value.clone())
}

/// ORG is a TextListProperty (semicolon-delimited components per
/// RFC 6350 § 6.6.4). We join with "; " for human-readable output.
pub fn org_value(c: &Vcard) -> Option<String> {
    c.org.first().map(|p| p.value.join("; "))
}

fn email_value(p: &TextProperty) -> String {
    p.value.clone()
}

pub fn first_email(c: &Vcard) -> Option<String> {
    c.email.first().map(email_value)
}

pub fn all_emails(c: &Vcard) -> Vec<String> {
    c.email.iter().map(email_value).collect()
}

fn tel_value(p: &TextOrUriProperty) -> String {
    match p {
        TextOrUriProperty::Text(t) => t.value.clone(),
        TextOrUriProperty::Uri(u) => uri_value(u),
    }
}

pub fn first_phone(c: &Vcard) -> Option<String> {
    c.tel.first().map(tel_value)
}

pub fn all_phones(c: &Vcard) -> Vec<String> {
    c.tel.iter().map(tel_value).collect()
}

fn uri_value(p: &UriProperty) -> String {
    // UriProperty Display delegates to fluent_uri / similar; the
    // most stable surface is just to format the Uri field.
    format!("{}", p.value)
}

pub fn first_url(c: &Vcard) -> Option<String> {
    c.url.first().map(uri_value)
}

/// BDAY → ISO 8601 extended form (YYYY-MM-DD), folding the basic
/// form vcard4 emits ("YYYYMMDD") into the canonical hyphenated
/// form. Time-of-birth and timezone (rare in BDAY) preserved as-is.
pub fn birthday(c: &Vcard) -> Option<String> {
    let bday = c.bday.as_ref()?;
    match bday {
        DateTimeOrTextProperty::DateTime(p) => {
            let part = p.value.first()?;
            Some(date_or_time_iso(part))
        }
        // Some implementations emit free-form text BDAY (e.g. "circa
        // 1985") — surface verbatim.
        DateTimeOrTextProperty::Text(t) => Some(t.value.clone()),
    }
}

fn date_or_time_iso(v: &DateAndOrTime) -> String {
    // vcard4 Display uses the "basic" ISO 8601 form (no separators).
    // Convert dates to the extended form for readability; pass
    // datetimes through (they're already explicit).
    match v {
        DateAndOrTime::Date(_) => {
            let basic = format!("{}", v); // e.g. "19850615"
            iso_basic_date_to_extended(&basic).unwrap_or(basic)
        }
        _ => format!("{}", v),
    }
}

/// "19850615" → "1985-06-15". Anything else → None.
fn iso_basic_date_to_extended(s: &str) -> Option<String> {
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        Some(format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..]))
    } else if s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
    {
        // Already extended.
        Some(s.to_string())
    } else {
        None
    }
}

// ─────────────── addresses ───────────────

fn address_json(a: &AddressProperty) -> serde_json::Value {
    let d = &a.value;
    serde_json::json!({
        "po_box":           d.po_box.as_deref().unwrap_or(""),
        "extended_address": d.extended_address.as_deref().unwrap_or(""),
        "street":           d.street_address.as_deref().unwrap_or(""),
        "locality":         d.locality.as_deref().unwrap_or(""),
        "region":           d.region.as_deref().unwrap_or(""),
        "postal_code":      d.postal_code.as_deref().unwrap_or(""),
        "country":          d.country_name.as_deref().unwrap_or(""),
    })
}

pub fn addresses_json(c: &Vcard) -> String {
    let arr: Vec<serde_json::Value> = c.address.iter().map(address_json).collect();
    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
}

// ─────────────── all-fields JSON ───────────────

pub fn all_json(c: &Vcard, raw: &str) -> String {
    let mut obj = serde_json::Map::new();
    if let Some(v) = version_in(raw) {
        obj.insert("version".to_string(), serde_json::Value::String(v));
    }
    if let Some(v) = fn_value(c) {
        obj.insert("fn".to_string(), serde_json::Value::String(v));
    }
    if let Some(n) = &c.name {
        // N is "family;given;additional;prefix;suffix" per RFC 6350 § 6.2.2.
        let parts: Vec<serde_json::Value> = n
            .value
            .iter()
            .map(|s| serde_json::Value::String(s.clone()))
            .collect();
        obj.insert("n".to_string(), serde_json::Value::Array(parts));
    }
    let emails = all_emails(c);
    if !emails.is_empty() {
        obj.insert(
            "emails".to_string(),
            serde_json::Value::Array(
                emails.into_iter().map(serde_json::Value::String).collect(),
            ),
        );
    }
    let phones = all_phones(c);
    if !phones.is_empty() {
        obj.insert(
            "phones".to_string(),
            serde_json::Value::Array(
                phones.into_iter().map(serde_json::Value::String).collect(),
            ),
        );
    }
    if let Some(v) = org_value(c) {
        obj.insert("org".to_string(), serde_json::Value::String(v));
    }
    if let Some(v) = title_value(c) {
        obj.insert("title".to_string(), serde_json::Value::String(v));
    }
    if !c.address.is_empty() {
        let arr: Vec<serde_json::Value> = c.address.iter().map(address_json).collect();
        obj.insert("addresses".to_string(), serde_json::Value::Array(arr));
    }
    if let Some(v) = birthday(c) {
        obj.insert("birthday".to_string(), serde_json::Value::String(v));
    }
    if let Some(v) = first_url(c) {
        obj.insert("url".to_string(), serde_json::Value::String(v));
    }
    if let Some(v) = note_value(c) {
        obj.insert("note".to_string(), serde_json::Value::String(v));
    }
    serde_json::to_string(&serde_json::Value::Object(obj))
        .unwrap_or_else(|_| "{}".to_string())
}

// ─────────────── wasm component export ───────────────

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

    const FID_FN: u64 = 1;
    const FID_EMAIL: u64 = 2;
    const FID_EMAILS: u64 = 3;
    const FID_PHONE: u64 = 4;
    const FID_PHONES: u64 = 5;
    const FID_ORG: u64 = 6;
    const FID_TITLE: u64 = 7;
    const FID_ADDRESSES: u64 = 8;
    const FID_BIRTHDAY: u64 = 9;
    const FID_URL: u64 = 10;
    const FID_NOTE: u64 = 11;
    const FID_VERSION_IN: u64 = 12;
    const FID_ALL: u64 = 13;
    const FID_VERSION: u64 = 14;

    struct Ext;

    fn opt_text(v: Option<String>) -> SqlValue {
        v.map(SqlValue::Text).unwrap_or(SqlValue::Null)
    }

    fn json_array(v: Vec<String>) -> SqlValue {
        if v.is_empty() {
            return SqlValue::Null;
        }
        let arr: Vec<serde_json::Value> =
            v.into_iter().map(serde_json::Value::String).collect();
        match serde_json::to_string(&arr) {
            Ok(s) => SqlValue::Text(s),
            Err(_) => SqlValue::Null,
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
                name: "vcard".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FN, "vcard_fn", 1),
                    s(FID_EMAIL, "vcard_email", 1),
                    s(FID_EMAILS, "vcard_emails", 1),
                    s(FID_PHONE, "vcard_phone", 1),
                    s(FID_PHONES, "vcard_phones", 1),
                    s(FID_ORG, "vcard_org", 1),
                    s(FID_TITLE, "vcard_title", 1),
                    s(FID_ADDRESSES, "vcard_addresses", 1),
                    s(FID_BIRTHDAY, "vcard_birthday", 1),
                    s(FID_URL, "vcard_url", 1),
                    s(FID_NOTE, "vcard_note", 1),
                    s(FID_VERSION_IN, "vcard_version_in", 1),
                    s(FID_ALL, "vcard_all", 1),
                    s(FID_VERSION, "vcard_version", 0),
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
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }
            // Every other fn is single-arg TEXT in. NULL input ⇒ NULL out.
            let t = match args.first() {
                Some(SqlValue::Text(s)) => s.clone(),
                Some(SqlValue::Null) | None => return Ok(SqlValue::Null),
                _ => return Err(format!("vcard: arg 0 must be TEXT")),
            };

            // vcard_version_in works without a successful full parse —
            // it scans the raw input.
            if func_id == FID_VERSION_IN {
                return Ok(opt_text(super::version_in(&t)));
            }

            let card = match super::parse_first(&t) {
                Some(c) => c,
                None => return Ok(SqlValue::Null),
            };

            match func_id {
                FID_FN => Ok(opt_text(super::fn_value(&card))),
                FID_EMAIL => Ok(opt_text(super::first_email(&card))),
                FID_EMAILS => Ok(json_array(super::all_emails(&card))),
                FID_PHONE => Ok(opt_text(super::first_phone(&card))),
                FID_PHONES => Ok(json_array(super::all_phones(&card))),
                FID_ORG => Ok(opt_text(super::org_value(&card))),
                FID_TITLE => Ok(opt_text(super::title_value(&card))),
                FID_ADDRESSES => {
                    if card.address.is_empty() {
                        Ok(SqlValue::Null)
                    } else {
                        Ok(SqlValue::Text(super::addresses_json(&card)))
                    }
                }
                FID_BIRTHDAY => Ok(opt_text(super::birthday(&card))),
                FID_URL => Ok(opt_text(super::first_url(&card))),
                FID_NOTE => Ok(opt_text(super::note_value(&card))),
                FID_ALL => Ok(SqlValue::Text(super::all_json(&card, &t))),
                other => Err(format!("vcard: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}

// `vec![]` macro is used inside the wasm export module; also ensure
// the import is exercised for non-wasm builds (e.g. cargo check).
#[allow(dead_code)]
fn _vec_import_witness() -> Vec<u8> {
    vec![]
}
