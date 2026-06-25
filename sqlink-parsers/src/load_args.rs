//! `.load PATH [--grant=cap[,cap,...]] [--allowed-hosts=...]
//! [--allowed-domains=...]` parser.
//!
//! Canonical source for both `sqlink-native` (the native loader
//! binary) and the `parse_load_args` fuzz target. Behaviour
//! preserved verbatim from the historical inline implementation at
//! `sqlink-native/src/main.rs` so policy invariants tracked by the
//! fuzz target stay meaningful.

use anyhow::{anyhow, Result};
use sqlite_extension_policy::{Capability, DnsPolicy, HttpPolicy, Policy};
use std::string::{String, ToString};
use std::vec::Vec;

/// Parse `.load PATH [--grant=...] [--allowed-hosts=...]
/// [--allowed-domains=...]`. Returns `(path, policy)`.
///
/// Unknown `--flag=value` pairs are silently ignored to keep
/// parity with the wasm cli's tolerant argv (which accepts
/// `--fuel`, `--epoch`, `--mem`, `--trust` etc. that the native
/// loader doesn't model).
pub fn parse_load_args(input: &str) -> Result<(String, Policy)> {
    let mut parts = input.split_whitespace();
    let path = parts
        .next()
        .ok_or_else(|| anyhow!(".load: missing path"))?
        .to_string();

    let mut grants: Vec<Capability> = Vec::new();
    let mut allowed_hosts: Vec<String> = Vec::new();
    let mut allowed_domains: Vec<String> = Vec::new();

    for arg in parts {
        let Some((k, v)) = arg.split_once('=') else {
            return Err(anyhow!(".load: expected --key=value, got {arg:?}"));
        };
        match k {
            "--grant" => {
                for cap in v.split(',') {
                    let cap = cap.trim();
                    if cap.is_empty() {
                        continue;
                    }
                    let c = match cap.to_ascii_lowercase().as_str() {
                        "spi" => Capability::Spi,
                        "prepared" => Capability::Prepared,
                        "transaction" => Capability::Transaction,
                        "schema" => Capability::Schema,
                        "state" => Capability::State,
                        "cache" => Capability::Cache,
                        "random" => Capability::Random,
                        "text" => Capability::Text,
                        "hashing" => Capability::Hashing,
                        "encoding" => Capability::Encoding,
                        "http" => Capability::Http,
                        "dns" => Capability::Dns,
                        "wal-frames" | "wal_frames" => Capability::WalFrames,
                        "s3" => Capability::S3,
                        "spawn-build" | "spawn_build" => Capability::SpawnBuild,
                        other => return Err(anyhow!(".load: unknown grant {other:?}")),
                    };
                    grants.push(c);
                }
            }
            "--allowed-hosts" => {
                for h in v.split(',') {
                    let h = h.trim();
                    if !h.is_empty() {
                        allowed_hosts.push(h.to_string());
                    }
                }
            }
            "--allowed-domains" => {
                for d in v.split(',') {
                    let d = d.trim();
                    if !d.is_empty() {
                        allowed_domains.push(d.to_string());
                    }
                }
            }
            _ => {
                // Unknown flags non-fatal  wasm cli accepts
                // --fuel/--epoch/--mem/--trust that we ignore.
            }
        }
    }

    let http = if grants.iter().any(|c| *c == Capability::Http) {
        Some(HttpPolicy {
            allowed_hosts,
            allowed_methods: None,
            max_body_bytes: None,
            timeout_ms: None,
        })
    } else {
        None
    };
    let dns = if grants.iter().any(|c| *c == Capability::Dns) {
        Some(DnsPolicy {
            allowed_domains,
            timeout_ms: None,
        })
    } else {
        None
    };

    let mut policy = Policy::deny_all().with_grants(grants);
    if let Some(h) = http {
        policy = policy.with_http(h);
    }
    if let Some(d) = dns {
        policy = policy.with_dns(d);
    }
    Ok((path, policy))
}

#[cfg(test)]
mod tests {
    use super::parse_load_args;
    use sqlite_extension_policy::Capability;
    use std::string::ToString;
    use std::vec;

    #[test]
    fn just_path() {
        let (p, pol) = parse_load_args("/tmp/x.wasm").unwrap();
        assert_eq!(p, "/tmp/x.wasm");
        assert!(!pol.is_granted(Capability::Spi));
    }

    #[test]
    fn missing_path() {
        let e = parse_load_args("").unwrap_err().to_string();
        assert!(e.contains("missing path"));
    }

    #[test]
    fn single_grant() {
        let (_p, pol) = parse_load_args("/x --grant=spi").unwrap();
        assert!(pol.is_granted(Capability::Spi));
    }

    #[test]
    fn multi_grant_comma_separated() {
        let (_p, pol) = parse_load_args("/x --grant=spi,http,dns").unwrap();
        assert!(pol.is_granted(Capability::Spi));
        assert!(pol.is_granted(Capability::Http));
        assert!(pol.is_granted(Capability::Dns));
    }

    #[test]
    fn underscore_or_dash_for_wal_frames() {
        let (_, p1) = parse_load_args("/x --grant=wal-frames").unwrap();
        let (_, p2) = parse_load_args("/x --grant=wal_frames").unwrap();
        assert!(p1.is_granted(Capability::WalFrames));
        assert!(p2.is_granted(Capability::WalFrames));
    }

    #[test]
    fn unknown_grant() {
        let e = parse_load_args("/x --grant=nope").unwrap_err().to_string();
        assert!(e.contains("unknown grant"));
    }

    #[test]
    fn http_side_policy_only_when_http_granted() {
        let (_, p) = parse_load_args("/x --grant=spi --allowed-hosts=a,b").unwrap();
        assert!(p.http.is_none());
        let (_, p) = parse_load_args("/x --grant=http --allowed-hosts=a,b").unwrap();
        let h = p.http.unwrap();
        assert_eq!(h.allowed_hosts, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn dns_side_policy_only_when_dns_granted() {
        let (_, p) = parse_load_args("/x --grant=dns --allowed-domains=e.com").unwrap();
        let d = p.dns.unwrap();
        assert_eq!(d.allowed_domains, vec!["e.com".to_string()]);
    }

    #[test]
    fn unknown_flag_silently_ignored() {
        // --fuel etc. is intentionally a no-op for the native loader.
        let (_, _) = parse_load_args("/x --fuel=1000 --epoch=ms").unwrap();
    }

    #[test]
    fn flag_without_equals_errors() {
        let e = parse_load_args("/x --grant").unwrap_err().to_string();
        assert!(e.contains("expected --key=value"));
    }
}
