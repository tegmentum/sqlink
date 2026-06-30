//! Task #226 — drive the REWIRED host dispatch through provider-backing.
//!
//! `Host::load_extension_as_provider` registers an `<ext>-provider.wasm`
//! as the backing for an extension and records which tiers were moved
//! onto the compose:dynlink provider. `Host::dispatch_scalar` and
//! `Host::dispatch_collation` then route through the provider's
//! `endpoint.handle` (woco envelope) instead of the bespoke per-world
//! cached Stores — this is the actual `do_load`-side rewiring, scoped to
//! the stateless tiers that are safe over the fresh-store boundary.
//!
//! The safety gate (vtab/hook/aggregate -> reject) is also exercised:
//! such extensions are refused for provider-backing and must stay on the
//! bespoke loader.

use std::path::PathBuf;

use ciborium::value::Value as Cbor;
use sqlink_host::compose_provider::ProviderHandle;
use sqlink_host::Host;

fn provider_path(name: &str) -> Option<PathBuf> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/providers");
    p.push(name);
    p.exists().then_some(p)
}

/// Read the first `<tier>[0].id` from the provider's `describe` manifest
/// so the test uses the real func/collation id the host stored (these
/// are the woco manifest ids, identical to what dispatch routes on).
async fn first_id(provider: &ProviderHandle, tier: &str) -> u64 {
    let bytes = provider.invoke("describe", &[]).await.expect("describe");
    let m: Cbor = ciborium::de::from_reader(&*bytes).unwrap();
    let Cbor::Map(map) = &m else { panic!("manifest not a map") };
    let entries = map
        .iter()
        .find(|(k, _)| matches!(k, Cbor::Text(s) if s == tier))
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no tier {tier}"));
    let Cbor::Array(a) = entries else { panic!("{tier} not array") };
    let first = &a[0];
    let Cbor::Map(fm) = first else { panic!("entry not map") };
    let id = fm
        .iter()
        .find(|(k, _)| matches!(k, Cbor::Text(s) if s == "id"))
        .map(|(_, v)| v)
        .expect("id field");
    match id {
        Cbor::Integer(i) => {
            let n: i128 = (*i).into();
            n as u64
        }
        _ => panic!("id not int"),
    }
}

fn backed_host(ext: &str, file: &str) -> Option<Host> {
    let path = provider_path(file)?;
    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_wasm_component(host.engine().clone(), path)
        .unwrap_or_else(|e| panic!("compile {file}: {e}"));
    let name = tokio::runtime::Handle::current()
        .block_on(host.load_extension_as_provider(ext, provider))
        .unwrap_or_else(|e| panic!("provider-back {ext}: {e}"));
    eprintln!("[{ext}] provider-backed (manifest name={name})");
    Some(host)
}

/// SCALAR tier routed through the provider via Host::dispatch_scalar.
#[tokio::test(flavor = "multi_thread")]
async fn dispatch_scalar_via_provider() {
    use sqlink_host::bindings::sqlite::extension::types::SqlValue;

    let Some(path) = provider_path("aba-provider.wasm") else {
        eprintln!("skip: aba-provider.wasm absent");
        return;
    };
    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_wasm_component(host.engine().clone(), path).unwrap();
    let func_id = first_id(&provider, "scalars").await;
    let name = host
        .load_extension_as_provider("aba", provider)
        .await
        .expect("provider-back aba");
    assert_eq!(name, "aba");

    // dispatch_scalar must route through the provider and return a value.
    let out = host
        .dispatch_scalar("aba", func_id, vec![SqlValue::Text("hello".into())])
        .await
        .expect("dispatch_scalar plumbing");
    match out {
        Ok(v) => eprintln!("[aba] dispatch_scalar(0, \"hello\") -> {v:?}"),
        Err(e) => panic!("aba scalar errored: {e}"),
    }
}

/// COLLATION tier routed through the provider via Host::dispatch_collation.
#[tokio::test(flavor = "multi_thread")]
async fn dispatch_collation_via_provider() {
    let Some(path) = provider_path("uint-provider.wasm") else {
        eprintln!("skip: uint-provider.wasm absent");
        return;
    };
    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_wasm_component(host.engine().clone(), path).unwrap();
    let coll_id = first_id(&provider, "collations").await;
    host.load_extension_as_provider("uint", provider)
        .await
        .expect("provider-back uint");

    // uint collation: "10" > "9" numerically.
    let ord = host
        .dispatch_collation("uint", coll_id, "10", "9")
        .await
        .expect("dispatch_collation plumbing");
    eprintln!("[uint] dispatch_collation(\"10\",\"9\") = {ord}");
    assert_eq!(ord, 1, "uint collation orders 10 > 9");
}

/// Safety gate: a vtab/hook/aggregate extension is REFUSED for
/// provider-backing so it falls back to the bespoke loader. count_min
/// (aggregate) and series (vtab) must both be rejected.
#[tokio::test(flavor = "multi_thread")]
async fn provider_backing_rejects_coherence_sensitive_tiers() {
    for (ext, file) in [
        ("count_min", "count_min-provider.wasm"),
        ("series", "series-provider.wasm"),
        ("inmem", "inmem-provider.wasm"),
        ("hookcb", "hookcb-provider.wasm"),
    ] {
        let Some(path) = provider_path(file) else {
            eprintln!("skip {file}: absent");
            continue;
        };
        let host = Host::new().unwrap();
        let provider = ProviderHandle::new_wasm_component(host.engine().clone(), path).unwrap();
        let res = host.load_extension_as_provider(ext, provider).await;
        assert!(
            res.is_err(),
            "{ext} declares a coherence-sensitive tier and must be \
             refused for provider-backing (falls back to bespoke loader)"
        );
        eprintln!("[{ext}] correctly refused: {}", res.unwrap_err());
    }
}

// silence unused warning when no fixtures present
#[allow(dead_code)]
fn _use(_: Option<Host>) {}
#[allow(dead_code)]
fn _mk() -> Option<Host> {
    backed_host("aba", "aba-provider.wasm")
}
