// build.rs: compress the IEEE OUI MA-L CSV into a compact, sorted
// `HHHHHH|Vendor Name\n`-per-line table.
//
// The raw CSV is ~3.7 MB and has four columns: Registry, Assignment,
// Organization Name, Organization Address. We only want
// (Assignment, Organization Name) and only for MA-L (the 24-bit OUI
// space; MA-M/MA-S are 28-/36-bit and don't map cleanly to "first 3
// octets" lookup). The output table is sorted by prefix (binary
// search at runtime) and lives in OUT_DIR/oui_table.txt.
//
// Address is dropped to keep the in-binary footprint near the
// ~1 MB target documented in PLAN-more-extensions-2.md - 8.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let csv_path = "oui.csv";
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", csv_path);

    let raw = fs::read_to_string(csv_path)
        .unwrap_or_else(|e| panic!("read {}: {}", csv_path, e));

    let mut rows: Vec<(String, String)> = Vec::new();
    for (lineno, line) in raw.lines().enumerate() {
        if lineno == 0 {
            // header
            continue;
        }
        let Some((registry, rest)) = line.split_once(',') else {
            continue;
        };
        if registry != "MA-L" {
            continue;
        }
        // Assignment is the next field; Organization Name follows.
        // Organization Name may be quoted (commas inside).
        let Some((assignment, rest2)) = rest.split_once(',') else {
            continue;
        };
        let assignment = assignment.trim().to_ascii_uppercase();
        if assignment.len() != 6 || !assignment.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        // Pull the (possibly-quoted) Organization Name field off rest2.
        let vendor = if let Some(rest3) = rest2.strip_prefix('"') {
            // up to next `","` boundary or `",` end-of-quote
            if let Some(end) = rest3.find('"') {
                &rest3[..end]
            } else {
                continue;
            }
        } else {
            // unquoted: ends at the next comma
            match rest2.split_once(',') {
                Some((v, _)) => v,
                None => rest2,
            }
        };
        let vendor = vendor.trim();
        if vendor.is_empty() {
            continue;
        }
        // Defensive: drop any embedded `|` / `\n` so the runtime parser
        // can use those as record/field separators without escaping.
        if vendor.contains('|') || vendor.contains('\n') {
            continue;
        }
        rows.push((assignment, vendor.to_string()));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup_by(|a, b| a.0 == b.0);

    let mut out = String::with_capacity(rows.len() * 40);
    for (pfx, vendor) in &rows {
        out.push_str(pfx);
        out.push('|');
        out.push_str(vendor);
        out.push('\n');
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let out_path = out_dir.join("oui_table.txt");
    fs::write(&out_path, &out)
        .unwrap_or_else(|e| panic!("write {}: {}", out_path.display(), e));

    // Also emit a small constant so the runtime side knows how many
    // entries it has (handy for logging and the version scalar).
    let count_path = out_dir.join("oui_count.txt");
    fs::write(&count_path, format!("{}", rows.len()))
        .unwrap_or_else(|e| panic!("write {}: {}", count_path.display(), e));

    eprintln!(
        "mac-oui build.rs: wrote {} MA-L entries to {} ({} bytes)",
        rows.len(),
        out_path.display(),
        out.len(),
    );
}
