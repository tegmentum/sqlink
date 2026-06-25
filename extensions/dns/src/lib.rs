//! DNS resolver scalar wired against `host-spi.dns`.

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
            world: "minimal-dns",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Capability, Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::dns::{self, RecordType};
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_RESOLVE: u64 = 1;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse_record_type(s: &str) -> RecordType {
        match s.to_ascii_uppercase().as_str() {
            "A" => RecordType::A,
            "AAAA" => RecordType::Aaaa,
            "CNAME" => RecordType::Cname,
            "MX" => RecordType::Mx,
            "NS" => RecordType::Ns,
            "TXT" => RecordType::Txt,
            "PTR" => RecordType::Ptr,
            "SOA" => RecordType::Soa,
            "SRV" => RecordType::Srv,
            other => RecordType::Other(other.to_string()),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "dns".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![ScalarFunctionSpec {
                    id: FID_RESOLVE,
                    name: "dns_resolve".to_string(),
                    num_args: 2,
                    func_flags: FunctionFlags::empty(),
                }],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![Capability::Dns],
                optional_capabilities: alloc::vec![],
                preferred_prefix: Some("dns".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.dns".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_RESOLVE => {
                    let name = arg_text(&args, 0, "dns_resolve")?;
                    let rtype_str = arg_text(&args, 1, "dns_resolve")?;
                    let rtype = parse_record_type(&rtype_str);
                    match dns::resolve(&name, &rtype) {
                        Ok(records) => Ok(SqlValue::Text(
                            serde_json::to_string(&records).unwrap_or_else(|_| "[]".to_string()),
                        )),
                        Err(dns::DnsError::Nxdomain) => Ok(SqlValue::Text("[]".to_string())),
                        Err(dns::DnsError::Refused(msg)) => {
                            Err(format!("dns_resolve refused: {msg}"))
                        }
                        Err(dns::DnsError::TimedOut) => {
                            Err("dns_resolve: lookup timed out".to_string())
                        }
                        Err(dns::DnsError::Other(msg)) => Err(format!("dns_resolve: {msg}")),
                    }
                }
                other => Err(format!("dns: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
