//! Embed path for country. Lookup table duplicated from wasm_export.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_NAME: u64 = 1;
const FID_ALPHA2: u64 = 2;
const FID_ALPHA3: u64 = 3;
const FID_NUMERIC: u64 = 4;
const FID_REGION: u64 = 5;

type Entry = (&'static str, &'static str, u16, &'static str, &'static str);

const TABLE: &[Entry] = &[
    ("AE", "ARE", 784, "United Arab Emirates", "Asia"),
    ("AF", "AFG", 4, "Afghanistan", "Asia"),
    ("AL", "ALB", 8, "Albania", "Europe"),
    ("AM", "ARM", 51, "Armenia", "Asia"),
    ("AR", "ARG", 32, "Argentina", "Americas"),
    ("AT", "AUT", 40, "Austria", "Europe"),
    ("AU", "AUS", 36, "Australia", "Oceania"),
    ("AZ", "AZE", 31, "Azerbaijan", "Asia"),
    ("BA", "BIH", 70, "Bosnia and Herzegovina", "Europe"),
    ("BD", "BGD", 50, "Bangladesh", "Asia"),
    ("BE", "BEL", 56, "Belgium", "Europe"),
    ("BG", "BGR", 100, "Bulgaria", "Europe"),
    ("BH", "BHR", 48, "Bahrain", "Asia"),
    ("BO", "BOL", 68, "Bolivia", "Americas"),
    ("BR", "BRA", 76, "Brazil", "Americas"),
    ("BY", "BLR", 112, "Belarus", "Europe"),
    ("CA", "CAN", 124, "Canada", "Americas"),
    ("CH", "CHE", 756, "Switzerland", "Europe"),
    ("CL", "CHL", 152, "Chile", "Americas"),
    ("CN", "CHN", 156, "China", "Asia"),
    ("CO", "COL", 170, "Colombia", "Americas"),
    ("CR", "CRI", 188, "Costa Rica", "Americas"),
    ("CU", "CUB", 192, "Cuba", "Americas"),
    ("CY", "CYP", 196, "Cyprus", "Europe"),
    ("CZ", "CZE", 203, "Czech Republic", "Europe"),
    ("DE", "DEU", 276, "Germany", "Europe"),
    ("DK", "DNK", 208, "Denmark", "Europe"),
    ("DO", "DOM", 214, "Dominican Republic", "Americas"),
    ("EC", "ECU", 218, "Ecuador", "Americas"),
    ("EE", "EST", 233, "Estonia", "Europe"),
    ("EG", "EGY", 818, "Egypt", "Africa"),
    ("ES", "ESP", 724, "Spain", "Europe"),
    ("ET", "ETH", 231, "Ethiopia", "Africa"),
    ("FI", "FIN", 246, "Finland", "Europe"),
    ("FR", "FRA", 250, "France", "Europe"),
    ("GB", "GBR", 826, "United Kingdom", "Europe"),
    ("GE", "GEO", 268, "Georgia", "Asia"),
    ("GH", "GHA", 288, "Ghana", "Africa"),
    ("GR", "GRC", 300, "Greece", "Europe"),
    ("GT", "GTM", 320, "Guatemala", "Americas"),
    ("HK", "HKG", 344, "Hong Kong", "Asia"),
    ("HN", "HND", 340, "Honduras", "Americas"),
    ("HR", "HRV", 191, "Croatia", "Europe"),
    ("HU", "HUN", 348, "Hungary", "Europe"),
    ("ID", "IDN", 360, "Indonesia", "Asia"),
    ("IE", "IRL", 372, "Ireland", "Europe"),
    ("IL", "ISR", 376, "Israel", "Asia"),
    ("IN", "IND", 356, "India", "Asia"),
    ("IQ", "IRQ", 368, "Iraq", "Asia"),
    ("IR", "IRN", 364, "Iran", "Asia"),
    ("IS", "ISL", 352, "Iceland", "Europe"),
    ("IT", "ITA", 380, "Italy", "Europe"),
    ("JM", "JAM", 388, "Jamaica", "Americas"),
    ("JO", "JOR", 400, "Jordan", "Asia"),
    ("JP", "JPN", 392, "Japan", "Asia"),
    ("KE", "KEN", 404, "Kenya", "Africa"),
    ("KG", "KGZ", 417, "Kyrgyzstan", "Asia"),
    ("KH", "KHM", 116, "Cambodia", "Asia"),
    ("KR", "KOR", 410, "South Korea", "Asia"),
    ("KW", "KWT", 414, "Kuwait", "Asia"),
    ("KZ", "KAZ", 398, "Kazakhstan", "Asia"),
    ("LB", "LBN", 422, "Lebanon", "Asia"),
    ("LK", "LKA", 144, "Sri Lanka", "Asia"),
    ("LT", "LTU", 440, "Lithuania", "Europe"),
    ("LU", "LUX", 442, "Luxembourg", "Europe"),
    ("LV", "LVA", 428, "Latvia", "Europe"),
    ("MA", "MAR", 504, "Morocco", "Africa"),
    ("MX", "MEX", 484, "Mexico", "Americas"),
    ("MY", "MYS", 458, "Malaysia", "Asia"),
    ("NG", "NGA", 566, "Nigeria", "Africa"),
    ("NL", "NLD", 528, "Netherlands", "Europe"),
    ("NO", "NOR", 578, "Norway", "Europe"),
    ("NP", "NPL", 524, "Nepal", "Asia"),
    ("NZ", "NZL", 554, "New Zealand", "Oceania"),
    ("OM", "OMN", 512, "Oman", "Asia"),
    ("PE", "PER", 604, "Peru", "Americas"),
    ("PH", "PHL", 608, "Philippines", "Asia"),
    ("PK", "PAK", 586, "Pakistan", "Asia"),
    ("PL", "POL", 616, "Poland", "Europe"),
    ("PT", "PRT", 620, "Portugal", "Europe"),
    ("QA", "QAT", 634, "Qatar", "Asia"),
    ("RO", "ROU", 642, "Romania", "Europe"),
    ("RS", "SRB", 688, "Serbia", "Europe"),
    ("RU", "RUS", 643, "Russia", "Europe"),
    ("SA", "SAU", 682, "Saudi Arabia", "Asia"),
    ("SE", "SWE", 752, "Sweden", "Europe"),
    ("SG", "SGP", 702, "Singapore", "Asia"),
    ("SI", "SVN", 705, "Slovenia", "Europe"),
    ("SK", "SVK", 703, "Slovakia", "Europe"),
    ("SY", "SYR", 760, "Syria", "Asia"),
    ("TH", "THA", 764, "Thailand", "Asia"),
    ("TN", "TUN", 788, "Tunisia", "Africa"),
    ("TR", "TUR", 792, "Turkey", "Asia"),
    ("TW", "TWN", 158, "Taiwan", "Asia"),
    ("TZ", "TZA", 834, "Tanzania", "Africa"),
    ("UA", "UKR", 804, "Ukraine", "Europe"),
    ("UG", "UGA", 800, "Uganda", "Africa"),
    ("US", "USA", 840, "United States", "Americas"),
    ("UY", "URY", 858, "Uruguay", "Americas"),
    ("UZ", "UZB", 860, "Uzbekistan", "Asia"),
    ("VE", "VEN", 862, "Venezuela", "Americas"),
    ("VN", "VNM", 704, "Vietnam", "Asia"),
    ("YE", "YEM", 887, "Yemen", "Asia"),
    ("ZA", "ZAF", 710, "South Africa", "Africa"),
    ("ZW", "ZWE", 716, "Zimbabwe", "Africa"),
];

fn lookup(raw: &str) -> Option<&'static Entry> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u16>() {
        return TABLE.iter().find(|e| e.2 == n);
    }
    if !s.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let upper: String = s.chars().map(|c| c.to_ascii_uppercase()).collect();
    match upper.len() {
        2 => TABLE.iter().find(|e| e.0 == upper),
        3 => TABLE.iter().find(|e| e.1 == upper),
        _ => None,
    }
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "country")?;
    let entry = lookup(&raw);
    Ok(match func_id {
        FID_NAME => entry
            .map(|e| SqlValueOwned::Text(e.3.to_string()))
            .unwrap_or(SqlValueOwned::Null),
        FID_ALPHA2 => entry
            .map(|e| SqlValueOwned::Text(e.0.to_string()))
            .unwrap_or(SqlValueOwned::Null),
        FID_ALPHA3 => entry
            .map(|e| SqlValueOwned::Text(e.1.to_string()))
            .unwrap_or(SqlValueOwned::Null),
        FID_NUMERIC => entry
            .map(|e| SqlValueOwned::Integer(e.2 as i64))
            .unwrap_or(SqlValueOwned::Null),
        FID_REGION => entry
            .map(|e| SqlValueOwned::Text(e.4.to_string()))
            .unwrap_or(SqlValueOwned::Null),
        other => return Err(format!("country: unknown func id {other}")),
    })
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_NAME,
        name: b"country_name\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ALPHA2,
        name: b"country_alpha2\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ALPHA3,
        name: b"country_alpha3\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NUMERIC,
        name: b"country_numeric\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_REGION,
        name: b"country_region\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
