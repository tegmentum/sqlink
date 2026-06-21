//! Cross-DB system / identity scalars. BI tools and ORMs probe
//! with `SELECT version()` / `SELECT current_user` to identify
//! the engine. SQLite has no real notion of users, roles, or
//! multiple schemas, so we return sensible constants:
//!
//!   user / current_user / session_user / system_user  'sqlink'
//!   current_role                                      ''
//!   current_database / database                       'main'
//!   current_schema   / schema                         'main'
//!   current_schemas(include_temp)                     'main' or 'main,temp'
//!   version()                                         'SQLink/<v> (SQLite <v>)'
//!   collation(text)                                   'BINARY'
//!   format_bytes(n)                                   e.g. '1.21 MiB'
//!
//! The point is portability, not authenticity  if a tool sees a
//! non-null answer it stops asking. Anything that actually needs
//! the answer to be true (auth, mtls, RLS) should not use this
//! extension.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

mod algo {
    use alloc::format;
    use alloc::string::{String, ToString};

    pub const IDENTITY: &str = "sqlink";
    pub const SCHEMA: &str = "main";

    pub fn version_string() -> String {
        format!("SQLink/{} (SQLite {})", env!("CARGO_PKG_VERSION"), bundled_sqlite_version())
    }

    /// Baked-in bundled SQLite version. The cli's libsqlite3-sys
    /// dep is the source of truth; bump this string when that dep
    /// upgrades. Kept const so the wasm path doesn't need to link
    /// libsqlite3-sys.
    pub fn bundled_sqlite_version() -> &'static str {
        "3.50.0"
    }

    pub fn current_schemas(include_temp: bool) -> String {
        if include_temp { "main,temp".to_string() } else { SCHEMA.to_string() }
    }

    pub fn collation_of(_text: &str) -> &'static str {
        "BINARY"
    }

    /// Human-readable byte sizes  binary base, two decimals on
    /// non-byte units. Mirrors MariaDB's FORMAT_BYTES.
    pub fn format_bytes(n: i64) -> String {
        let abs = n.unsigned_abs() as f64;
        let neg = if n < 0 { "-" } else { "" };
        const KIB: f64 = 1024.0;
        const MIB: f64 = KIB * 1024.0;
        const GIB: f64 = MIB * 1024.0;
        const TIB: f64 = GIB * 1024.0;
        const PIB: f64 = TIB * 1024.0;
        if abs < KIB        { format!("{}{} bytes", neg, abs as i64) }
        else if abs < MIB   { format!("{}{:.2} KiB", neg, abs / KIB) }
        else if abs < GIB   { format!("{}{:.2} MiB", neg, abs / MIB) }
        else if abs < TIB   { format!("{}{:.2} GiB", neg, abs / GIB) }
        else if abs < PIB   { format!("{}{:.2} TiB", neg, abs / TIB) }
        else                { format!("{}{:.2} PiB", neg, abs / PIB) }
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use crate::algo;
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

    pub const FID_USER:             u64 = 1;
    pub const FID_CURRENT_USER:     u64 = 2;
    pub const FID_SESSION_USER:     u64 = 3;
    pub const FID_SYSTEM_USER:      u64 = 4;
    pub const FID_CURRENT_ROLE:     u64 = 5;
    pub const FID_DATABASE:         u64 = 6;
    pub const FID_CURRENT_DATABASE: u64 = 7;
    pub const FID_SCHEMA:           u64 = 8;
    pub const FID_CURRENT_SCHEMA:   u64 = 9;
    pub const FID_CURRENT_SCHEMAS:  u64 = 10;
    pub const FID_VERSION:          u64 = 11;
    pub const FID_COLLATION:        u64 = 12;
    pub const FID_FORMAT_BYTES:     u64 = 13;

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
                name: "sys-compat".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_USER,             "user",             0),
                    s(FID_CURRENT_USER,     "current_user",     0),
                    s(FID_SESSION_USER,     "session_user",     0),
                    s(FID_SYSTEM_USER,      "system_user",      0),
                    s(FID_CURRENT_ROLE,     "current_role",     0),
                    s(FID_DATABASE,         "database",         0),
                    s(FID_CURRENT_DATABASE, "current_database", 0),
                    s(FID_SCHEMA,           "schema",           0),
                    s(FID_CURRENT_SCHEMA,   "current_schema",   0),
                    s(FID_CURRENT_SCHEMAS,  "current_schemas",  1),
                    s(FID_VERSION,          "version",          0),
                    s(FID_COLLATION,        "collation",        1),
                    s(FID_FORMAT_BYTES,     "format_bytes",     1),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_USER | FID_CURRENT_USER | FID_SESSION_USER | FID_SYSTEM_USER => {
                    Ok(SqlValue::Text(algo::IDENTITY.to_string()))
                }
                FID_CURRENT_ROLE => Ok(SqlValue::Text(String::new())),
                FID_DATABASE | FID_CURRENT_DATABASE | FID_SCHEMA | FID_CURRENT_SCHEMA => {
                    Ok(SqlValue::Text(algo::SCHEMA.to_string()))
                }
                FID_CURRENT_SCHEMAS => {
                    let include_temp = match args.first() {
                        Some(SqlValue::Integer(n)) => *n != 0,
                        Some(SqlValue::Text(s)) => matches!(s.to_lowercase().as_str(), "true" | "1" | "yes"),
                        _ => false,
                    };
                    Ok(SqlValue::Text(algo::current_schemas(include_temp)))
                }
                FID_VERSION => Ok(SqlValue::Text(algo::version_string())),
                FID_COLLATION => {
                    let s = match args.first() {
                        Some(SqlValue::Text(t)) => t.as_str(),
                        Some(SqlValue::Blob(_)) => return Ok(SqlValue::Text("BINARY".to_string())),
                        _ => "",
                    };
                    Ok(SqlValue::Text(algo::collation_of(s).to_string()))
                }
                FID_FORMAT_BYTES => {
                    let n = match args.first() {
                        Some(SqlValue::Integer(n)) => *n,
                        Some(SqlValue::Real(r)) => *r as i64,
                        Some(SqlValue::Text(s)) => s.parse::<i64>().map_err(|_| "format_bytes: not integer".to_string())?,
                        _ => return Err("format_bytes: INTEGER arg".to_string()),
                    };
                    Ok(SqlValue::Text(algo::format_bytes(n)))
                }
                other => Err(format!("sys-compat: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
