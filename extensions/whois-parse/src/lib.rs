//! whois-parse — parse WHOIS *response text* (not a network lookup).
//!
//! WHOIS responses are notoriously irregular; every registrar and RIR
//! invented its own line shape. After surveying ARIN / RIPE / APNIC /
//! LACNIC / AFRINIC / GoDaddy / MarkMonitor / Verisign / Network
//! Solutions output we settled on three line shapes the parser must
//! tolerate:
//!
//!   1. `key: value`           the classic RFC-3912-ish shape
//!   2. `key = value`          older GoDaddy + a few country ccTLDs
//!   3. `key   value`          whitespace-separated, ARIN's preferred
//!                             output for the "v6 view" tables
//!
//! Keys are case-insensitive and may contain spaces (`Name Server`,
//! `Registrar IANA ID`). Values may span multiple lines when the
//! upstream wraps them; we keep the first-line value and append
//! continuation lines on subsequent identical keys (registrars
//! commonly repeat `Name Server:` once per nameserver, so that
//! "append" behaviour is exactly the join we need for the JSON
//! array).
//!
//! Date normalisation is intentionally narrow: we handle the four
//! formats that cover >95% of WHOIS responses in the wild and
//! leave anything else as-is. The contract is "ISO 8601 if
//! parseable" — falling through verbatim is documented as the
//! fallback so a caller can still diff what they got.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ─────────────── line-shape parser ───────────────

/// Split one WHOIS line into (key, value) if the line carries any of
/// the three shapes documented in the module docs. Returns `None`
/// for comment lines (`%`, `#`, `>>>`), blanks, and the "footer"
/// disclaimer wrap that most RIRs append.
///
/// Why not regex? The three shapes are simple enough that a
/// hand-rolled split is half the binary size and reads more
/// obviously when you're tracing a "why didn't this key match"
/// bug at 2am.
pub fn split_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Standard WHOIS comment lead-ins. RIPE uses `%`, ARIN uses `#`,
    // Verisign sometimes echoes `>>> Last update of WHOIS database`.
    if trimmed.starts_with('%')
        || trimmed.starts_with('#')
        || trimmed.starts_with(">>>")
    {
        return None;
    }

    // Shape 1: key:value. Take the FIRST colon so values containing
    // ':' (URLs, IPv6 addrs) survive intact.
    if let Some(idx) = trimmed.find(':') {
        // Guard: a leading colon (`:` at idx 0) isn't a key/value line.
        if idx > 0 {
            let key = trimmed[..idx].trim().to_string();
            let value = trimmed[idx + 1..].trim().to_string();
            // Guard: pure URL ("http://example.com") would parse as
            // key="http", value="//example.com". A key with whitespace
            // INSIDE it (no, the key never has whitespace before the
            // colon in any sane format) is fine; but we reject keys
            // that look like URL schemes. Conservative check: key
            // must contain at least one ASCII letter and no '/'.
            if !key.is_empty()
                && !key.contains('/')
                && key.chars().any(|c| c.is_ascii_alphabetic())
            {
                return Some((key, value));
            }
        }
    }

    // Shape 2: key=value. Older format; same first-occurrence rule.
    if let Some(idx) = trimmed.find('=') {
        if idx > 0 {
            let key = trimmed[..idx].trim().to_string();
            let value = trimmed[idx + 1..].trim().to_string();
            if !key.is_empty()
                && key.chars().any(|c| c.is_ascii_alphabetic())
            {
                return Some((key, value));
            }
        }
    }

    // Shape 3: key   value (≥2 whitespace chars). This is the
    // looseest shape and easiest to misfire on, so we require a
    // run of ≥2 whitespace characters as the separator. A single
    // space would catch every prose line in the disclaimer.
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    // Find first run of ≥2 whitespace after at least one non-ws char.
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i == 0 || i >= bytes.len() {
        return None;
    }
    let key_end = i;
    let mut ws_run = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        ws_run += 1;
        i += 1;
    }
    if ws_run < 2 || i >= bytes.len() {
        return None;
    }
    let key = trimmed[..key_end].trim().to_string();
    let value = trimmed[i..].trim().to_string();
    if key.is_empty() || !key.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    // Shape-3 fallback must still look like a key: short, no digits-only.
    if key.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((key, value))
}

/// Parse the whole WHOIS text into an ordered list of (key, value).
/// Keys are returned in original case; downstream callers fold case
/// when matching. We keep duplicates (Name Server: appears 4× for a
/// 4-NS domain) — the field-lookup helpers decide whether to take
/// the first match or aggregate.
pub fn parse_pairs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(pair) = split_line(line) {
            out.push(pair);
        }
    }
    out
}

// ─────────────── public field-lookup helpers ───────────────

/// Case-insensitive lookup of a field. Returns the FIRST matching
/// value verbatim; matches whois_field's contract.
pub fn field(text: &str, name: &str) -> Option<String> {
    let want = name.trim().to_ascii_lowercase();
    for (k, v) in parse_pairs(text) {
        if k.trim().to_ascii_lowercase() == want {
            return Some(v);
        }
    }
    None
}

/// Try to extract a registrar name. Registries label this field
/// differently; we walk a small priority list of synonyms.
pub fn registrar(text: &str) -> Option<String> {
    for syn in [
        "Registrar",
        "Sponsoring Registrar",
        "Registrar Name",
        "Source Registry",
    ] {
        if let Some(v) = field(text, syn) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Pull a date-like field and normalise to ISO 8601 (YYYY-MM-DD) if
/// it matches one of the formats we recognise. Falls through to the
/// raw value if not — callers can detect "did this parse?" by
/// checking strict format equality.
pub fn creation_date(text: &str) -> Option<String> {
    for syn in [
        "Creation Date",
        "Created",
        "Created On",
        "Domain Registration Date",
        "Registered On",
        "Registration Time",
        "created",
    ] {
        if let Some(v) = field(text, syn) {
            if !v.is_empty() {
                return Some(normalise_date(&v));
            }
        }
    }
    None
}

pub fn expiration_date(text: &str) -> Option<String> {
    for syn in [
        "Registry Expiry Date",
        "Registrar Registration Expiration Date",
        "Expiration Date",
        "Expires On",
        "Expiry Date",
        "Expires",
        "paid-till",
    ] {
        if let Some(v) = field(text, syn) {
            if !v.is_empty() {
                return Some(normalise_date(&v));
            }
        }
    }
    None
}

/// Collect every name-server value (multi-valued field), lowercased
/// and de-duplicated while preserving first-seen order.
pub fn name_servers(text: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for (k, v) in parse_pairs(text) {
        let kn = k.trim().to_ascii_lowercase();
        if kn == "name server" || kn == "nserver" || kn == "nameserver" || kn == "name servers" {
            // ARIN's "v6 view" lists NS as a space-separated tuple of
            // `host  ip` — keep just the host token.
            let host = v.split_whitespace().next().unwrap_or("").trim_end_matches('.');
            if host.is_empty() {
                continue;
            }
            let host_lower = host.to_ascii_lowercase();
            if !seen.iter().any(|h| h == &host_lower) {
                seen.push(host_lower);
            }
        }
    }
    seen
}

/// Aggregate all parsed pairs into a single JSON object. Duplicates
/// fold by joining values with `\n` so the JSON object stays flat
/// and predictable for SQL consumers.
pub fn parse_object(text: &str) -> serde_json::Value {
    use serde_json::{Map, Value};
    let mut map: Map<String, Value> = Map::new();
    for (k, v) in parse_pairs(text) {
        // Preserve original (trimmed) case on first sight; collapse
        // case-equivalent dupes onto the first key seen.
        let lower = k.trim().to_ascii_lowercase();
        let existing_key = map
            .keys()
            .find(|kk| kk.trim().to_ascii_lowercase() == lower)
            .cloned();
        match existing_key {
            Some(real_key) => {
                let entry = map.get_mut(&real_key).unwrap();
                if let Value::String(s) = entry {
                    s.push('\n');
                    s.push_str(&v);
                }
            }
            None => {
                map.insert(k.trim().to_string(), Value::String(v));
            }
        }
    }
    Value::Object(map)
}

// ─────────────── date normalisation ───────────────

/// Best-effort ISO 8601 normaliser. Handles:
///   * already ISO 8601:  2023-04-15 or 2023-04-15T10:00:00Z
///   * dotted ISO:        2023.04.15
///   * slash ISO:         2023/04/15
///   * DD-Mon-YYYY:       15-Apr-2023  (Verisign legacy)
///   * DD.MM.YYYY:        15.04.2023   (DENIC + .ru)
///
/// Returns the input verbatim when nothing matches; this is by
/// design so callers can still diff strange formats they haven't
/// seen.
pub fn normalise_date(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }

    // 1. Already ISO-leading: keep first 10 chars iff they look like
    //    a yyyy-mm-dd date. Anything after is the time/zone portion;
    //    contract returns the date so we drop it.
    let bytes = s.as_bytes();
    if bytes.len() >= 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
    {
        return s[..10].to_string();
    }

    // 2. Dotted or slashed: YYYY[./]MM[./]DD  swap separators.
    if bytes.len() >= 10
        && (bytes[4] == b'.' || bytes[4] == b'/')
        && (bytes[7] == b'.' || bytes[7] == b'/')
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
    {
        return format!("{}-{}-{}", &s[..4], &s[5..7], &s[8..10]);
    }

    // 3. DD-Mon-YYYY (Verisign style, e.g. 15-Apr-2023).
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() >= 3 {
        if let (Ok(d), Some(m), Ok(y)) = (
            parts[0].parse::<u32>(),
            month_name_to_num(parts[1]),
            parts[2].split(|c: char| !c.is_ascii_digit()).next().unwrap_or("").parse::<u32>(),
        ) {
            if (1..=31).contains(&d) && (1900..=2999).contains(&y) {
                return format!("{:04}-{:02}-{:02}", y, m, d);
            }
        }
    }

    // 4. DD.MM.YYYY (DENIC, .ru).
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() >= 3 {
        if let (Ok(d), Ok(m), Ok(y)) = (
            parts[0].parse::<u32>(),
            parts[1].parse::<u32>(),
            parts[2].split(|c: char| !c.is_ascii_digit()).next().unwrap_or("").parse::<u32>(),
        ) {
            if (1..=31).contains(&d) && (1..=12).contains(&m) && (1900..=2999).contains(&y) {
                return format!("{:04}-{:02}-{:02}", y, m, d);
            }
        }
    }

    raw.to_string()
}

fn month_name_to_num(s: &str) -> Option<u32> {
    let lower = s.trim().to_ascii_lowercase();
    Some(match lower.as_str() {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    })
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

    const FID_FIELD: u64 = 1;
    const FID_REGISTRAR: u64 = 2;
    const FID_CREATION: u64 = 3;
    const FID_EXPIRATION: u64 = 4;
    const FID_NAMESERVERS: u64 = 5;
    const FID_PARSE: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn opt_text_or_null(v: Option<String>) -> SqlValue {
        match v {
            Some(s) => SqlValue::Text(s),
            None => SqlValue::Null,
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
                name: "whois_parse".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FIELD, "whois_field", 2, det),
                    s(FID_REGISTRAR, "whois_registrar", 1, det),
                    s(FID_CREATION, "whois_creation_date", 1, det),
                    s(FID_EXPIRATION, "whois_expiration_date", 1, det),
                    s(FID_NAMESERVERS, "whois_name_servers", 1, det),
                    s(FID_PARSE, "whois_parse", 1, det),
                    s(FID_VERSION, "whois_version", 0, det),
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
            // NULL propagation: NULL TEXT arg ⇒ NULL output, applied
            // before any per-id parsing. Saves us from threading
            // Option<String> through every helper.
            if matches!(args.first(), Some(SqlValue::Null)) && func_id != FID_VERSION {
                return Ok(SqlValue::Null);
            }
            match func_id {
                FID_FIELD => {
                    if matches!(args.get(1), Some(SqlValue::Null)) {
                        return Ok(SqlValue::Null);
                    }
                    let text = arg_text(&args, 0, "whois_field")?;
                    let name = arg_text(&args, 1, "whois_field")?;
                    Ok(opt_text_or_null(super::field(&text, &name)))
                }
                FID_REGISTRAR => {
                    let text = arg_text(&args, 0, "whois_registrar")?;
                    Ok(opt_text_or_null(super::registrar(&text)))
                }
                FID_CREATION => {
                    let text = arg_text(&args, 0, "whois_creation_date")?;
                    Ok(opt_text_or_null(super::creation_date(&text)))
                }
                FID_EXPIRATION => {
                    let text = arg_text(&args, 0, "whois_expiration_date")?;
                    Ok(opt_text_or_null(super::expiration_date(&text)))
                }
                FID_NAMESERVERS => {
                    let text = arg_text(&args, 0, "whois_name_servers")?;
                    let arr = super::name_servers(&text);
                    Ok(SqlValue::Text(
                        serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string()),
                    ))
                }
                FID_PARSE => {
                    let text = arg_text(&args, 0, "whois_parse")?;
                    let obj = super::parse_object(&text);
                    Ok(SqlValue::Text(
                        serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string()),
                    ))
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("whois_parse: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
