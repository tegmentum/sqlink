//! IP address / CIDR scalar helpers.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::{String, ToString};
use core::net::IpAddr;
use core::str::FromStr;
use ipnet::IpNet;

pub fn family(addr: &str) -> Result<i64, String> {
    // Accept bare addresses and CIDRs; family is determined
    // by the host bits' parsing.
    if let Ok(ip) = IpAddr::from_str(addr) {
        return Ok(if ip.is_ipv4() { 4 } else { 6 });
    }
    let net = IpNet::from_str(addr).map_err(|e| alloc::format!("ip_family: {e}"))?;
    Ok(match net {
        IpNet::V4(_) => 4,
        IpNet::V6(_) => 6,
    })
}

pub fn in_cidr(addr: &str, cidr: &str) -> Result<bool, String> {
    let ip = IpAddr::from_str(addr).map_err(|e| alloc::format!("ip_in_cidr: addr: {e}"))?;
    let net = IpNet::from_str(cidr).map_err(|e| alloc::format!("ip_in_cidr: cidr: {e}"))?;
    Ok(net.contains(&ip))
}

pub fn host(cidr: &str) -> Result<String, String> {
    let net = IpNet::from_str(cidr).map_err(|e| alloc::format!("ip_host: {e}"))?;
    Ok(net.addr().to_string())
}

pub fn network(cidr: &str) -> Result<String, String> {
    let net = IpNet::from_str(cidr).map_err(|e| alloc::format!("ip_network: {e}"))?;
    Ok(net.network().to_string())
}

pub fn broadcast(cidr: &str) -> Result<String, String> {
    let net = IpNet::from_str(cidr).map_err(|e| alloc::format!("ip_broadcast: {e}"))?;
    Ok(net.broadcast().to_string())
}

pub fn prefix_len(cidr: &str) -> Result<i64, String> {
    let net = IpNet::from_str(cidr).map_err(|e| alloc::format!("ip_prefix_len: {e}"))?;
    Ok(net.prefix_len() as i64)
}

pub fn contains(a: &str, b: &str) -> Result<bool, String> {
    let na = IpNet::from_str(a).map_err(|e| alloc::format!("ip_contains: a: {e}"))?;
    let nb = IpNet::from_str(b).map_err(|e| alloc::format!("ip_contains: b: {e}"))?;
    Ok(na.contains(&nb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_v4_and_v6() {
        assert_eq!(family("192.168.1.1").unwrap(), 4);
        assert_eq!(family("::1").unwrap(), 6);
        assert_eq!(family("10.0.0.0/8").unwrap(), 4);
        assert_eq!(family("2001:db8::/32").unwrap(), 6);
    }

    #[test]
    fn in_cidr_matches_v4() {
        assert!(in_cidr("10.5.4.3", "10.0.0.0/8").unwrap());
        assert!(!in_cidr("11.5.4.3", "10.0.0.0/8").unwrap());
    }

    #[test]
    fn network_and_broadcast_v4() {
        assert_eq!(network("192.168.1.5/24").unwrap(), "192.168.1.0");
        assert_eq!(broadcast("192.168.1.5/24").unwrap(), "192.168.1.255");
    }

    #[test]
    fn contains_supernet() {
        assert!(contains("10.0.0.0/8", "10.5.0.0/16").unwrap());
        assert!(!contains("10.5.0.0/16", "10.0.0.0/8").unwrap());
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

    const FID_FAMILY: u64 = 1;
    const FID_IN_CIDR: u64 = 2;
    const FID_HOST: u64 = 3;
    const FID_NETWORK: u64 = 4;
    const FID_BROADCAST: u64 = 5;
    const FID_PREFIX_LEN: u64 = 6;
    const FID_CONTAINS: u64 = 7;

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
                name: "ipaddr".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FAMILY, "ip_family", 1),
                    s(FID_IN_CIDR, "ip_in_cidr", 2),
                    s(FID_HOST, "ip_host", 1),
                    s(FID_NETWORK, "ip_network", 1),
                    s(FID_BROADCAST, "ip_broadcast", 1),
                    s(FID_PREFIX_LEN, "ip_prefix_len", 1),
                    s(FID_CONTAINS, "ip_contains", 2),
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

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_FAMILY => {
                    super::family(&arg_text(&args, 0, "ip_family")?)
                        .map(SqlValue::Integer)
                }
                FID_IN_CIDR => super::in_cidr(
                    &arg_text(&args, 0, "ip_in_cidr")?,
                    &arg_text(&args, 1, "ip_in_cidr")?,
                )
                .map(|b| SqlValue::Integer(b as i64)),
                FID_HOST => {
                    super::host(&arg_text(&args, 0, "ip_host")?).map(SqlValue::Text)
                }
                FID_NETWORK => {
                    super::network(&arg_text(&args, 0, "ip_network")?).map(SqlValue::Text)
                }
                FID_BROADCAST => super::broadcast(&arg_text(&args, 0, "ip_broadcast")?)
                    .map(SqlValue::Text),
                FID_PREFIX_LEN => {
                    super::prefix_len(&arg_text(&args, 0, "ip_prefix_len")?)
                        .map(SqlValue::Integer)
                }
                FID_CONTAINS => super::contains(
                    &arg_text(&args, 0, "ip_contains")?,
                    &arg_text(&args, 1, "ip_contains")?,
                )
                .map(|b| SqlValue::Integer(b as i64)),
                other => Err(format!("ipaddr: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
