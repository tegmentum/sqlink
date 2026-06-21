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

/// MySQL `INET_ATON(addr)`  IPv4 dotted-quad string to 32-bit
/// integer (big-endian).
pub fn inet_aton(addr: &str) -> Result<i64, String> {
    match IpAddr::from_str(addr) {
        Ok(IpAddr::V4(v4)) => Ok(u32::from(v4) as i64),
        Ok(IpAddr::V6(_)) => Err("inet_aton: IPv6 not supported  use inet6_aton".to_string()),
        Err(e) => Err(alloc::format!("inet_aton: {e}")),
    }
}

/// MySQL `INET_NTOA(n)`  inverse of `inet_aton`.
pub fn inet_ntoa(n: i64) -> Result<String, String> {
    if !(0..=u32::MAX as i64).contains(&n) {
        return Err("inet_ntoa: out of u32 range".to_string());
    }
    Ok(core::net::Ipv4Addr::from(n as u32).to_string())
}

/// MySQL `INET6_ATON(addr)`  IPv4 or IPv6 text to BLOB (4 or
/// 16 bytes). Matches the canonical MySQL behaviour.
pub fn inet6_aton(addr: &str) -> Result<alloc::vec::Vec<u8>, String> {
    match IpAddr::from_str(addr) {
        Ok(IpAddr::V4(v4)) => Ok(v4.octets().to_vec()),
        Ok(IpAddr::V6(v6)) => Ok(v6.octets().to_vec()),
        Err(e) => Err(alloc::format!("inet6_aton: {e}")),
    }
}

/// MySQL `INET6_NTOA(b)`  BLOB to canonical text.
pub fn inet6_ntoa(b: &[u8]) -> Result<String, String> {
    match b.len() {
        4 => Ok(core::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string()),
        16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(b);
            Ok(core::net::Ipv6Addr::from(octets).to_string())
        }
        _ => Err(alloc::format!("inet6_ntoa: BLOB must be 4 or 16 bytes, got {}", b.len())),
    }
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
    // Cross-DB (MySQL-style) text↔binary converters.
    const FID_INET_ATON:  u64 = 8;
    const FID_INET_NTOA:  u64 = 9;
    const FID_INET6_ATON: u64 = 10;
    const FID_INET6_NTOA: u64 = 11;
    // MySQL / MariaDB / BigQuery checks:
    const FID_IS_IPV4:         u64 = 12;
    const FID_IS_IPV6:         u64 = 13;
    const FID_IS_IPV4_COMPAT:  u64 = 14;
    const FID_IS_IPV4_MAPPED:  u64 = 15;
    const FID_INET_SAME_FAMILY: u64 = 16;
    const FID_INET_MERGE:      u64 = 17;

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
                    s(FID_INET_ATON,  "inet_aton",  1),
                    s(FID_INET_NTOA,  "inet_ntoa",  1),
                    s(FID_INET6_ATON, "inet6_aton", 1),
                    s(FID_INET6_NTOA, "inet6_ntoa", 1),
                    s(FID_IS_IPV4, "is_ipv4", 1),
                    s(FID_IS_IPV6, "is_ipv6", 1),
                    s(FID_IS_IPV4_COMPAT, "is_ipv4_compat", 1),
                    s(FID_IS_IPV4_MAPPED, "is_ipv4_mapped", 1),
                    s(FID_INET_SAME_FAMILY, "inet_same_family", 2),
                    s(FID_INET_MERGE, "inet_merge", 2),
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
                FID_INET_ATON => super::inet_aton(
                    &arg_text(&args, 0, "inet_aton")?
                ).map(SqlValue::Integer),
                FID_INET_NTOA => {
                    let n = match args.first() {
                        Some(SqlValue::Integer(n)) => *n,
                        Some(SqlValue::Real(r)) => *r as i64,
                        _ => return Err("inet_ntoa: INTEGER arg".to_string()),
                    };
                    super::inet_ntoa(n).map(SqlValue::Text)
                }
                FID_INET6_ATON => super::inet6_aton(
                    &arg_text(&args, 0, "inet6_aton")?
                ).map(SqlValue::Blob),
                FID_INET6_NTOA => {
                    let bytes = match args.first() {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        Some(SqlValue::Text(s)) => s.as_bytes().to_vec(),
                        _ => return Err("inet6_ntoa: BLOB arg".to_string()),
                    };
                    super::inet6_ntoa(&bytes).map(SqlValue::Text)
                }
                FID_IS_IPV4 => {
                    let s = arg_text(&args, 0, "is_ipv4")?;
                    Ok(SqlValue::Integer(matches!(s.parse::<core::net::IpAddr>(), Ok(core::net::IpAddr::V4(_))) as i64))
                }
                FID_IS_IPV6 => {
                    let s = arg_text(&args, 0, "is_ipv6")?;
                    Ok(SqlValue::Integer(matches!(s.parse::<core::net::IpAddr>(), Ok(core::net::IpAddr::V6(_))) as i64))
                }
                FID_IS_IPV4_COMPAT => {
                    let s = arg_text(&args, 0, "is_ipv4_compat")?;
                    let r = if let Ok(core::net::IpAddr::V6(v6)) = s.parse::<core::net::IpAddr>() {
                        let segs = v6.segments();
                        segs[..6].iter().all(|&x| x == 0) && (segs[6] != 0 || segs[7] != 0)
                    } else { false };
                    Ok(SqlValue::Integer(r as i64))
                }
                FID_IS_IPV4_MAPPED => {
                    let s = arg_text(&args, 0, "is_ipv4_mapped")?;
                    let r = if let Ok(core::net::IpAddr::V6(v6)) = s.parse::<core::net::IpAddr>() {
                        v6.to_ipv4_mapped().is_some()
                    } else { false };
                    Ok(SqlValue::Integer(r as i64))
                }
                FID_INET_SAME_FAMILY => {
                    let a = arg_text(&args, 0, "inet_same_family")?;
                    let b = arg_text(&args, 1, "inet_same_family")?;
                    let r = match (a.parse::<core::net::IpAddr>(), b.parse::<core::net::IpAddr>()) {
                        (Ok(core::net::IpAddr::V4(_)), Ok(core::net::IpAddr::V4(_))) => true,
                        (Ok(core::net::IpAddr::V6(_)), Ok(core::net::IpAddr::V6(_))) => true,
                        _ => false,
                    };
                    Ok(SqlValue::Integer(r as i64))
                }
                FID_INET_MERGE => {
                    // PG inet_merge(a, b)  smallest network containing
                    // both. Compute by finding the common CIDR prefix.
                    let a = arg_text(&args, 0, "inet_merge")?;
                    let b = arg_text(&args, 1, "inet_merge")?;
                    let na = a.parse::<ipnet::IpNet>()
                        .map_err(|e| format!("inet_merge: a: {e}"))?;
                    let nb = b.parse::<ipnet::IpNet>()
                        .map_err(|e| format!("inet_merge: b: {e}"))?;
                    let max = na.prefix_len().min(nb.prefix_len());
                    let mut prefix = max;
                    while prefix > 0 {
                        let ta = ipnet::IpNet::new(na.addr(), prefix).ok();
                        let tb = ipnet::IpNet::new(nb.addr(), prefix).ok();
                        if let (Some(ta), Some(tb)) = (ta, tb) {
                            if ta.network() == tb.network() { break; }
                        }
                        prefix -= 1;
                    }
                    let merged = ipnet::IpNet::new(na.addr(), prefix)
                        .map_err(|e| format!("inet_merge: {e}"))?;
                    Ok(SqlValue::Text(merged.to_string()))
                }
                other => Err(format!("ipaddr: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
