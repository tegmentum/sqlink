#![no_main]
//! Fuzz the `.load PATH [--grant=...]...` argv parser.
//!
//! Source of truth: `sqlink-native/src/main.rs:136`
//! `fn parse_load_args(input: &str) -> Result<(String, Policy)>`.
//! sqlink-native is a binary crate (no library output for fuzz
//! deps), so the algorithm is COPIED here. If the source changes
//! shape, MIRROR IT here. v1.1 plan: split sqlink-native into a
//! lib+bin so this duplication isn't needed.
//!
//! Properties:
//!   1. Never panics for arbitrary &str.
//!   2. Policy invariant: every cap pushed to grants matches a
//!      known Capability variant; unknown grants error.
//!   3. http/dns side-policies only attach when the corresponding
//!      capability is in the grant list.

use libfuzzer_sys::fuzz_target;
use sqlite_extension_policy::{Capability, DnsPolicy, HttpPolicy, Policy};

#[derive(Debug)]
struct ParseError(String);

fn parse_load_args(input: &str) -> Result<(String, Policy), ParseError> {
    let mut parts = input.split_whitespace();
    let path = parts
        .next()
        .ok_or_else(|| ParseError(".load: missing path".into()))?
        .to_string();

    let mut grants: Vec<Capability> = Vec::new();
    let mut allowed_hosts: Vec<String> = Vec::new();
    let mut allowed_domains: Vec<String> = Vec::new();

    for arg in parts {
        let Some((k, v)) = arg.split_once('=') else {
            return Err(ParseError(format!(
                ".load: expected --key=value, got {arg:?}"
            )));
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
                        other => {
                            return Err(ParseError(format!(".load: unknown grant {other:?}")))
                        }
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
            _ => {}
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

    let mut policy = Policy::deny_all().with_grants(grants.iter().copied());
    if let Some(h) = http {
        policy = policy.with_http(h);
    }
    if let Some(d) = dns {
        policy = policy.with_dns(d);
    }

    Ok((path, policy))
}

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };

    let Ok((_path, policy)) = parse_load_args(s) else { return };

    // Side-policy invariant: http present iff Http in grants.
    let http_granted = policy.is_granted(Capability::Http);
    assert_eq!(http_granted, policy.http.is_some(),
        "http side-policy out of sync with grant for {s:?}");

    let dns_granted = policy.is_granted(Capability::Dns);
    assert_eq!(dns_granted, policy.dns.is_some(),
        "dns side-policy out of sync with grant for {s:?}");
});
