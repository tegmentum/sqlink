//! ISO 4217 currency code  name, symbol, decimals

extern crate alloc;

use alloc::string::String;

#[cfg(feature = "embed")]
pub mod embed;

/// (alpha-3 code, ISO 4217 numeric, decimals, symbol, English name).
/// Sourced from https://en.wikipedia.org/wiki/ISO_4217 (current
/// currencies only). Curated ~70 most-used; full list is 180+.
/// Decimals of 0 = no minor unit (JPY, KRW); 3 = mils (BHD, KWD).
pub const TABLE: &[(&str, u16, u8, &str, &str)] = &[
    // alpha numeric  dec sym   name
    ("AED", 784, 2, "د.إ",  "United Arab Emirates dirham"),
    ("AFN", 971, 2, "؋",   "Afghan afghani"),
    ("ALL",   8, 2, "L",   "Albanian lek"),
    ("AMD",  51, 2, "֏",   "Armenian dram"),
    ("ARS",  32, 2, "$",   "Argentine peso"),
    ("AUD",  36, 2, "$",   "Australian dollar"),
    ("BDT",  50, 2, "৳",   "Bangladeshi taka"),
    ("BGN", 975, 2, "лв",  "Bulgarian lev"),
    ("BHD",  48, 3, ".د.ب", "Bahraini dinar"),
    ("BRL", 986, 2, "R$",  "Brazilian real"),
    ("CAD", 124, 2, "$",   "Canadian dollar"),
    ("CHF", 756, 2, "Fr",  "Swiss franc"),
    ("CLP", 152, 0, "$",   "Chilean peso"),
    ("CNY", 156, 2, "¥",   "Renminbi"),
    ("COP", 170, 2, "$",   "Colombian peso"),
    ("CRC", 188, 2, "₡",   "Costa Rican colón"),
    ("CZK", 203, 2, "K",  "Czech koruna"),
    ("DKK", 208, 2, "kr",  "Danish krone"),
    ("DOP", 214, 2, "$",   "Dominican peso"),
    ("EGP", 818, 2, "E",  "Egyptian pound"),
    ("EUR", 978, 2, "",   "Euro"),
    ("GBP", 826, 2, "£",   "Pound sterling"),
    ("GHS", 936, 2, "",  "Ghanaian cedi"),
    ("HKD", 344, 2, "HK$", "Hong Kong dollar"),
    ("HUF", 348, 2, "Ft",  "Hungarian forint"),
    ("IDR", 360, 2, "Rp",  "Indonesian rupiah"),
    ("ILS", 376, 2, "",   "Israeli new shekel"),
    ("INR", 356, 2, "",   "Indian rupee"),
    ("IQD", 368, 3, "ع.د", "Iraqi dinar"),
    ("IRR", 364, 2, "",  "Iranian rial"),
    ("ISK", 352, 0, "kr",  "Icelandic króna"),
    ("JOD", 400, 3, "JD",  "Jordanian dinar"),
    ("JPY", 392, 0, "¥",   "Japanese yen"),
    ("KES", 404, 2, "KSh", "Kenyan shilling"),
    ("KRW", 410, 0, "",   "South Korean won"),
    ("KWD", 414, 3, "KD",  "Kuwaiti dinar"),
    ("KZT", 398, 2, "",   "Kazakhstani tenge"),
    ("LBP", 422, 2, "L",  "Lebanese pound"),
    ("MAD", 504, 2, "DH",  "Moroccan dirham"),
    ("MXN", 484, 2, "$",   "Mexican peso"),
    ("MYR", 458, 2, "RM",  "Malaysian ringgit"),
    ("NGN", 566, 2, "",   "Nigerian naira"),
    ("NOK", 578, 2, "kr",  "Norwegian krone"),
    ("NPR", 524, 2, "Rs",  "Nepalese rupee"),
    ("NZD", 554, 2, "$",   "New Zealand dollar"),
    ("OMR", 512, 3, "R.O", "Omani rial"),
    ("PEN", 604, 2, "S/",  "Peruvian sol"),
    ("PHP", 608, 2, "",   "Philippine peso"),
    ("PKR", 586, 2, "Rs",  "Pakistani rupee"),
    ("PLN", 985, 2, "z",  "Polish złoty"),
    ("QAR", 634, 2, "QR",  "Qatari riyal"),
    ("RON", 946, 2, "lei", "Romanian leu"),
    ("RUB", 643, 2, "",   "Russian ruble"),
    ("SAR", 682, 2, "",   "Saudi riyal"),
    ("SEK", 752, 2, "kr",  "Swedish krona"),
    ("SGD", 702, 2, "$",   "Singapore dollar"),
    ("THB", 764, 2, "",   "Thai baht"),
    ("TRY", 949, 2, "",   "Turkish lira"),
    ("TWD", 901, 2, "NT$", "New Taiwan dollar"),
    ("TZS", 834, 2, "TSh", "Tanzanian shilling"),
    ("UAH", 980, 2, "",   "Ukrainian hryvnia"),
    ("UGX", 800, 0, "USh", "Ugandan shilling"),
    ("USD", 840, 2, "$",   "United States dollar"),
    ("UYU", 858, 2, "$",   "Uruguayan peso"),
    ("UZS", 860, 2, "",  "Uzbekistani sum"),
    ("VES", 928, 2, "Bs.", "Venezuelan bolívar soberano"),
    ("VND", 704, 0, "",   "Vietnamese đồng"),
    ("XAF", 950, 0, "Fr",  "Central African CFA franc"),
    ("XOF", 952, 0, "Fr",  "West African CFA franc"),
    ("ZAR", 710, 2, "R",   "South African rand"),
    ("ZMW", 967, 2, "K",   "Zambian kwacha"),
];

/// Look up a 3-letter ISO 4217 alpha code (case-insensitive,
/// trimmed). Returns the table entry tuple or `None` if invalid /
/// not found.
pub fn lookup(code: &str) -> Option<&'static (&'static str, u16, u8, &'static str, &'static str)> {
    let c = code.trim();
    if c.len() != 3 || !c.chars().all(|x| x.is_ascii_alphabetic()) {
        return None;
    }
    let upper: String = c.chars().map(|x| x.to_ascii_uppercase()).collect();
    TABLE.iter().find(|e| e.0 == upper)
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

    use super::lookup;

    const FID_NAME: u64 = 1;
    const FID_SYMBOL: u64 = 2;
    const FID_DECIMALS: u64 = 3;
    const FID_NUMERIC: u64 = 4;

    struct Ext;

    // ---- Arg helpers ----
    // The Big Three; copy-pasted into every extension. The
    // scaffold ships them so you delete what you don't need.

    #[allow(dead_code)]
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Available flags  pass `det` for deterministic scalars
            // (most cases), `nd` for ones that produce different
            // output each call (rng / time-of-call / counter).
            #[allow(unused_variables)]
            let det = FunctionFlags::DETERMINISTIC;
            #[allow(unused_variables)]
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "currency".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NAME, "currency_name", 1, det),
                    s(FID_SYMBOL, "currency_symbol", 1, det),
                    s(FID_DECIMALS, "currency_decimals", 1, det),
                    s(FID_NUMERIC, "currency_numeric", 1, det),
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
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let code = arg_text(&args, 0, "currency")?;
            let entry = lookup(&code);
            Ok(match func_id {
                FID_NAME => entry
                    .map(|e| SqlValue::Text(e.4.to_string()))
                    .unwrap_or(SqlValue::Null),
                FID_SYMBOL => entry
                    .map(|e| SqlValue::Text(e.3.to_string()))
                    .unwrap_or(SqlValue::Null),
                FID_DECIMALS => entry
                    .map(|e| SqlValue::Integer(e.2 as i64))
                    .unwrap_or(SqlValue::Null),
                FID_NUMERIC => entry
                    .map(|e| SqlValue::Integer(e.1 as i64))
                    .unwrap_or(SqlValue::Null),
                other => return Err(format!("currency: unknown func id {other}")),
            })
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
