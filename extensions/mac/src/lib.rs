//! MAC address (EUI-48) helper scalars not provided by `mac-oui`.
//!
//! After the duplicate-surface trim, this umbrella only exposes
//! the three helpers `mac-oui` does not:
//!   mac_is_local(s)     -> INTEGER bit 1 of octet 0
//!   mac_is_multicast(s) -> INTEGER bit 0 of octet 0
//!   mac_nic(s)          -> TEXT    "DDEEFF" (last 3 octets, uppercase)
//!
//! Validation / normalization / formatting / OUI extraction now live
//! in `extensions/mac-oui` (which also adds IEEE vendor lookup).

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

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

    const FID_NIC: u64 = 4;
    const FID_IS_MULTICAST: u64 = 5;
    const FID_IS_LOCAL: u64 = 6;

    struct Ext;

    /// Parse a MAC address, returning 6 raw bytes. Accepts any of:
    ///   colon-separated  AA:BB:CC:11:22:33
    ///   dash-separated   AA-BB-CC-11-22-33
    ///   dot-grouped       AABB.CC11.2233 (Cisco)
    ///   bare hex         AABBCC112233
    /// Case-insensitive. Returns None on any other shape.
    fn parse_mac(s: &str) -> Option<[u8; 6]> {
        let hex: String = s
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .collect();
        if hex.len() != 12 {
            return None;
        }
        let mut out = [0u8; 6];
        for i in 0..6 {
            out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(out)
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
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
                name: "mac".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NIC, "mac_nic", 1),
                    s(FID_IS_MULTICAST, "mac_is_multicast", 1),
                    s(FID_IS_LOCAL, "mac_is_local", 1),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "mac")?;
            let parsed = parse_mac(&raw);

            match func_id {
                FID_NIC => Ok(parsed
                    .map(|b| SqlValue::Text(alloc::format!(
                        "{:02X}{:02X}{:02X}", b[3], b[4], b[5]
                    )))
                    .unwrap_or(SqlValue::Null)),
                FID_IS_MULTICAST => Ok(parsed
                    .map(|b| SqlValue::Integer((b[0] & 0x01) as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_IS_LOCAL => Ok(parsed
                    .map(|b| SqlValue::Integer(((b[0] >> 1) & 0x01) as i64))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("mac: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
