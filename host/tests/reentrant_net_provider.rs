//! Task #220 — the http/dns provider instantiation proof.
//!
//! The `http`/`dns` extensions import `sqlite:extension/http` /
//! `sqlite:extension/dns` — capability surfaces the host provides. Like spi,
//! these static imports cannot be satisfied by composition, so the host wires
//! them onto the RESIDENT provider's linker (see
//! compose_provider::resident_wasm_component_invoke +
//! `impl loaded::sqlite::extension::http::Host for ProviderState` /
//! `loaded_minimal_dns::...::dns::Host for ProviderState`).
//!
//! This proves the import is SATISFIED: a `<ext>-provider.wasm` (the PLAIN
//! scalar shape wrapping the ext, leaving `sqlite:extension/{http,dns}`
//! imported) INSTANTIATES through the resident path — no "http/dns not found
//! in the linker". A live network round-trip is NOT asserted (sandboxed, and
//! the resident provider's policy is deny-by-default); instantiation + the
//! import being satisfied is the pass bar.
//!
//! Skips gracefully when the fixture is absent (built out-of-band by
//! `wac plug --plug <ext>_extension.wasm scalar-shape.wasm`).

use std::path::PathBuf;

use sqlink_host::compose_provider::ProviderHandle;
use sqlink_host::Host;

fn provider_path(name: &str) -> Option<PathBuf> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/providers");
    p.push(name);
    p.exists().then_some(p)
}

/// The host satisfies `sqlite:extension/http` on the resident linker: an
/// http-importing ext, wrapped in the plain scalar shape, instantiates.
#[tokio::test(flavor = "multi_thread")]
async fn resident_provider_satisfies_http_import() {
    let Some(path) = provider_path("http-provider.wasm") else {
        eprintln!("skip: http-provider.wasm absent (wac plug http into the scalar shape)");
        return;
    };
    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_resident_wasm_component(
        host.engine().clone(),
        path,
        None,
        String::new(),
    )
    .expect("compile http-provider");
    assert!(provider.is_resident());

    // `describe` forces the resident store to instantiate. Without the host
    // satisfying `sqlite:extension/http`, this fails with "http ... not found
    // in the linker".
    let bytes = provider
        .invoke("describe", &[])
        .await
        .expect("http-provider must instantiate — the host satisfies the http import");
    assert!(!bytes.is_empty(), "describe returned a manifest");
    eprintln!("[http] resident provider instantiated; http import satisfied by the host");
}

/// The host satisfies `sqlite:extension/dns` on the resident linker.
#[tokio::test(flavor = "multi_thread")]
async fn resident_provider_satisfies_dns_import() {
    let Some(path) = provider_path("dns-provider.wasm") else {
        eprintln!("skip: dns-provider.wasm absent (wac plug dns into the scalar shape)");
        return;
    };
    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_resident_wasm_component(
        host.engine().clone(),
        path,
        None,
        String::new(),
    )
    .expect("compile dns-provider");
    assert!(provider.is_resident());

    let bytes = provider
        .invoke("describe", &[])
        .await
        .expect("dns-provider must instantiate — the host satisfies the dns import");
    assert!(!bytes.is_empty(), "describe returned a manifest");
    eprintln!("[dns] resident provider instantiated; dns import satisfied by the host");
}
