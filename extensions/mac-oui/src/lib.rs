//! MAC address parsing + IEEE OUI vendor lookup.
//!
//! Plan: PLAN-more-extensions-2.md - 8 (Network track).
//!
//! Function surface:
//!   mac_is_valid(s)        -> INTEGER  1 if `s` parses as 6 hex octets
//!   mac_normalize(s)       -> TEXT     lowercase colon-separated
//!   mac_format(s, [style]) -> TEXT     style: 'colon'|'dash'|'dot'|'bare'
//!   mac_oui(s)             -> TEXT     "AABBCC" (3 octets uppercase)
//!   mac_vendor(s)          -> TEXT     IEEE OUI vendor, NULL if unknown
//!   mac_is_unicast(s)      -> INTEGER  bit 0 of octet 0 == 0
//!   mac_is_universal(s)    -> INTEGER  bit 1 of octet 0 == 0
//!   mac_random()           -> TEXT     random LAA address (U/L=1, I/G=0)
//!   mac_oui_version()      -> TEXT     crate version + OUI source tag
//!
//! Parser accepts: `AA:BB:CC:DD:EE:FF`, `AA-BB-CC-DD-EE-FF`,
//! `AABB.CCDD.EEFF` (Cisco), `AABBCCDDEEFF` (bare). Case-insensitive.
//! Any input that yields exactly 12 ASCII hex digits (after dropping
//! separators) is accepted - lenient by design, matching the legacy
//! `mac` extension. Returns NULL for any function that needs a parsed
//! address if the input doesn't parse.
//!
//! Vendor lookup uses the bundled MA-L slice of the IEEE OUI registry
//! (~30K vendors). The build.rs compresses oui.csv into a sorted
//! `PREFIX|Vendor\n`-per-line table; runtime does binary search.

extern crate alloc;

use alloc::string::String;

/// Bundled OUI table: `HHHHHH|Vendor Name\n`, sorted by prefix.
/// Built from extensions/mac-oui/oui.csv by build.rs.
const OUI_TABLE: &str = include_str!(concat!(env!("OUT_DIR"), "/oui_table.txt"));

/// Number of MA-L entries the build pulled out of the CSV. Stamped
/// into mac_oui_version() so callers can sanity-check the bundled db.
const OUI_COUNT: &str = include_str!(concat!(env!("OUT_DIR"), "/oui_count.txt"));

/// Parse any common MAC format into 6 raw bytes. Accepts colon,
/// dash, Cisco-dot, and bare-hex shapes interchangeably. Rejects
/// strings that carry stray non-separator characters - i.e. only
/// the four canonical separators (`:`, `-`, `.`, whitespace) may
/// sit between the hex digits.
fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut digits = [0u8; 12];
    let mut n = 0;
    for c in s.chars() {
        if c.is_ascii_hexdigit() {
            if n >= 12 {
                return None;
            }
            digits[n] = c as u8;
            n += 1;
        } else if matches!(c, ':' | '-' | '.' | ' ' | '\t') {
            // separators are fine
        } else {
            return None;
        }
    }
    if n != 12 {
        return None;
    }
    let mut out = [0u8; 6];
    for i in 0..6 {
        let hi = hex_val(digits[i * 2])?;
        let lo = hex_val(digits[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn format_with_sep(b: &[u8; 6], sep: char, upper: bool) -> String {
    let mut out = String::with_capacity(17);
    for (i, x) in b.iter().enumerate() {
        if i > 0 {
            out.push(sep);
        }
        if upper {
            out.push_str(&alloc::format!("{:02X}", x));
        } else {
            out.push_str(&alloc::format!("{:02x}", x));
        }
    }
    out
}

fn format_bare(b: &[u8; 6], upper: bool) -> String {
    let mut out = String::with_capacity(12);
    for x in b.iter() {
        if upper {
            out.push_str(&alloc::format!("{:02X}", x));
        } else {
            out.push_str(&alloc::format!("{:02x}", x));
        }
    }
    out
}

/// Cisco dot-grouped: `AABB.CCDD.EEFF` (3 groups of 4 hex digits).
fn format_dot(b: &[u8; 6], upper: bool) -> String {
    let mut out = String::with_capacity(14);
    for i in 0..3 {
        if i > 0 {
            out.push('.');
        }
        if upper {
            out.push_str(&alloc::format!("{:02X}{:02X}", b[i * 2], b[i * 2 + 1]));
        } else {
            out.push_str(&alloc::format!("{:02x}{:02x}", b[i * 2], b[i * 2 + 1]));
        }
    }
    out
}

/// Public so callers (and tests) can format any [u8;6] in any style.
/// Acceptance test expects style 'dash' to be uppercase ("AA-BB-...")
/// to match the IEEE-printed format.
pub fn format_style(b: &[u8; 6], style: &str) -> Option<String> {
    match style {
        "colon" | "" => Some(format_with_sep(b, ':', false)),
        "dash" => Some(format_with_sep(b, '-', true)),
        "dot" => Some(format_dot(b, false)),
        "bare" => Some(format_bare(b, false)),
        _ => None,
    }
}

/// Binary-search the embedded OUI table for the 6-char uppercase
/// prefix. Returns the vendor name, or None.
pub fn lookup_vendor(prefix_upper: &str) -> Option<&'static str> {
    debug_assert_eq!(prefix_upper.len(), 6);
    debug_assert!(prefix_upper.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));

    // Compute line offsets once would be ideal, but we'd need a static
    // lazy structure. Each line is variable length so we scan-by-line
    // with a binary search on byte offsets seeded at chunk midpoints.
    //
    // The table is ~1 MB / ~30K lines, so a 16-step binary search
    // each lookup is plenty fast. We do it on a slice of `\n`-aligned
    // line-start offsets computed lazily on first use.
    let starts = line_starts();
    let mut lo = 0usize;
    let mut hi = starts.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let off = starts[mid] as usize;
        let line_end = OUI_TABLE[off..]
            .find('\n')
            .map(|n| off + n)
            .unwrap_or(OUI_TABLE.len());
        let line = &OUI_TABLE[off..line_end];
        // `line` is "HHHHHH|Vendor Name"
        let (pfx, vendor) = match line.split_once('|') {
            Some(p) => p,
            None => return None,
        };
        match prefix_upper.cmp(pfx) {
            core::cmp::Ordering::Equal => return Some(vendor),
            core::cmp::Ordering::Less => hi = mid,
            core::cmp::Ordering::Greater => lo = mid + 1,
        }
    }
    None
}

/// Per-line starting byte offsets in OUI_TABLE. Computed once on
/// first use via OnceCell. Heap cost is ~30K * 8 = ~240 KB of u32s
/// (we keep them as u32 to halve that to ~120 KB; entries fit in
/// 24 bits since the table is ~1 MB).
fn line_starts() -> &'static [u32] {
    use core::sync::atomic::{AtomicBool, Ordering};
    static INIT: AtomicBool = AtomicBool::new(false);
    static mut STARTS: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    // Single-threaded in wasm component instances; we still gate on
    // an atomic to make the contract explicit.
    unsafe {
        if !INIT.load(Ordering::Acquire) {
            let mut v: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
            v.push(0);
            for (i, b) in OUI_TABLE.bytes().enumerate() {
                if b == b'\n' && i + 1 < OUI_TABLE.len() {
                    v.push((i + 1) as u32);
                }
            }
            STARTS = v;
            INIT.store(true, Ordering::Release);
        }
        let ptr = core::ptr::addr_of!(STARTS);
        let v: &alloc::vec::Vec<u32> = &*ptr;
        v.as_slice()
    }
}

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

    // Function ids. Two FIDs for mac_format to route 1-arg vs 2-arg.
    const FID_IS_VALID: u64 = 1;
    const FID_NORMALIZE: u64 = 2;
    const FID_FORMAT_1: u64 = 3;
    const FID_FORMAT_2: u64 = 4;
    const FID_OUI: u64 = 5;
    const FID_VENDOR: u64 = 6;
    const FID_IS_UNICAST: u64 = 7;
    const FID_IS_UNIVERSAL: u64 = 8;
    const FID_RANDOM: u64 = 9;
    const FID_VERSION: u64 = 10;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "mac-oui".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_IS_VALID, "mac_is_valid", 1, det),
                    s(FID_NORMALIZE, "mac_normalize", 1, det),
                    s(FID_FORMAT_1, "mac_format", 1, det),
                    s(FID_FORMAT_2, "mac_format", 2, det),
                    s(FID_OUI, "mac_oui", 1, det),
                    s(FID_VENDOR, "mac_vendor", 1, det),
                    s(FID_IS_UNICAST, "mac_is_unicast", 1, det),
                    s(FID_IS_UNIVERSAL, "mac_is_universal", 1, det),
                    s(FID_RANDOM, "mac_random", 0, nd),
                    s(FID_VERSION, "mac_oui_version", 0, det),
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
            // Functions with 0 args branch first so we don't run
            // arg_text on an empty arg list.
            match func_id {
                FID_RANDOM => {
                    let mut b = [0u8; 6];
                    getrandom::getrandom(&mut b)
                        .map_err(|e| format!("mac_random: getrandom: {e}"))?;
                    // Locally administered (U/L bit = 1), unicast (I/G = 0).
                    b[0] = (b[0] & 0b1111_1100) | 0b0000_0010;
                    return Ok(SqlValue::Text(super::format_with_sep(&b, ':', false)));
                }
                FID_VERSION => {
                    return Ok(SqlValue::Text(format!(
                        "mac-oui {} (IEEE OUI MA-L bundle, {} entries)",
                        env!("CARGO_PKG_VERSION"),
                        super::OUI_COUNT.trim(),
                    )));
                }
                _ => {}
            }

            let raw = arg_text(&args, 0, "mac-oui")?;
            let parsed = super::parse_mac(&raw);

            match func_id {
                FID_IS_VALID => Ok(SqlValue::Integer(parsed.is_some() as i64)),
                FID_NORMALIZE => Ok(parsed
                    .map(|b| SqlValue::Text(super::format_with_sep(&b, ':', false)))
                    .unwrap_or(SqlValue::Null)),
                FID_FORMAT_1 => Ok(parsed
                    .map(|b| SqlValue::Text(super::format_with_sep(&b, ':', false)))
                    .unwrap_or(SqlValue::Null)),
                FID_FORMAT_2 => {
                    let style = arg_text(&args, 1, "mac_format")?;
                    let Some(b) = parsed else {
                        return Ok(SqlValue::Null);
                    };
                    match super::format_style(&b, &style) {
                        Some(s) => Ok(SqlValue::Text(s)),
                        None => Err(format!(
                            "mac_format: unknown style {style:?}; want one of \
                             'colon', 'dash', 'dot', 'bare'"
                        )),
                    }
                }
                FID_OUI => Ok(parsed
                    .map(|b| {
                        SqlValue::Text(format!(
                            "{:02X}{:02X}{:02X}",
                            b[0], b[1], b[2]
                        ))
                    })
                    .unwrap_or(SqlValue::Null)),
                FID_VENDOR => {
                    let Some(b) = parsed else {
                        return Ok(SqlValue::Null);
                    };
                    let prefix = format!("{:02X}{:02X}{:02X}", b[0], b[1], b[2]);
                    match super::lookup_vendor(&prefix) {
                        Some(name) => Ok(SqlValue::Text(name.to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_IS_UNICAST => Ok(parsed
                    .map(|b| SqlValue::Integer((b[0] & 0x01 == 0) as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_IS_UNIVERSAL => Ok(parsed
                    .map(|b| SqlValue::Integer((b[0] & 0x02 == 0) as i64))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("mac-oui: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
