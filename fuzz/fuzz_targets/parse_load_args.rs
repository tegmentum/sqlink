#![no_main]
//! Fuzz the `.load PATH [--grant=...]...` argv parser.
//!
//! Source of truth lives at `sqlink-parsers/src/load_args.rs`
//! (`fn parse_load_args(input: &str) -> anyhow::Result<(String,
//! Policy)>`). sqlink-native imports it from there; this harness
//! does too.
//!
//! Properties:
//!   1. Never panics for arbitrary &str.
//!   2. Side-policy invariants: http present iff Http in grants,
//!      same for dns.

use libfuzzer_sys::fuzz_target;
use sqlink_parsers::load_args::parse_load_args;
use sqlite_extension_policy::Capability;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };

    let Ok((_path, policy)) = parse_load_args(s) else { return };

    // Side-policy invariant: http present iff Http in grants.
    let http_granted = policy.is_granted(Capability::Http);
    assert_eq!(
        http_granted,
        policy.http.is_some(),
        "http side-policy out of sync with grant for {s:?}"
    );

    let dns_granted = policy.is_granted(Capability::Dns);
    assert_eq!(
        dns_granted,
        policy.dns.is_some(),
        "dns side-policy out of sync with grant for {s:?}"
    );
});
