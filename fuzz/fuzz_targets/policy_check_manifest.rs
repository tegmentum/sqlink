#![no_main]
//! Fuzz `Policy::check_manifest`: the capability gate at the
//! load-time security boundary. Invariant under test:
//!
//!   declared ⊆ granted  ⇔  check_manifest(declared) == Ok(())
//!
//! Regressions here silently widen the trust surface (Gap E added
//! optional-capabilities; this target also exercises the simpler
//! required-only path that production code still relies on).

use libfuzzer_sys::fuzz_target;
use arbitrary::{Arbitrary, Unstructured};
use sqlite_extension_policy::{Capability, Policy};

// Capability doesn't derive Arbitrary upstream; map a u8 to the
// 16 variants enumerated in sqlite-loader-wit/src/lib.rs.
fn cap_from_byte(b: u8) -> Capability {
    match b % 16 {
        0 => Capability::Spi,
        1 => Capability::Prepared,
        2 => Capability::Transaction,
        3 => Capability::Schema,
        4 => Capability::State,
        5 => Capability::Cache,
        6 => Capability::Random,
        7 => Capability::Text,
        8 => Capability::Hashing,
        9 => Capability::Encoding,
        10 => Capability::Http,
        11 => Capability::Dns,
        12 => Capability::WalFrames,
        13 => Capability::S3,
        14 => Capability::SpawnBuild,
        _ => Capability::Bundles,
    }
}

#[derive(Arbitrary, Debug)]
struct Input {
    granted: Vec<u8>,
    declared: Vec<u8>,
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(input) = Input::arbitrary(&mut u) else { return };

    let granted: Vec<Capability> = input.granted.iter().map(|b| cap_from_byte(*b)).collect();
    let declared: Vec<Capability> = input.declared.iter().map(|b| cap_from_byte(*b)).collect();

    let policy = Policy::deny_all().with_grants(granted.iter().copied());
    let result = policy.check_manifest(&declared);

    // Invariant: all-declared-granted iff Ok.
    let all_granted = declared.iter().all(|c| policy.is_granted(*c));
    assert_eq!(result.is_ok(), all_granted,
        "check_manifest disagrees with is_granted: granted={granted:?} declared={declared:?} result={result:?}");
});
