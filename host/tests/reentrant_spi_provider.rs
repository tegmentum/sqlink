//! Task #220 — the reentrant-SPI provider e2e proof.
//!
//! An extension that imports `sqlite:extension/spi` (e.g. `eval`, which
//! runs runtime SQL via `spi.execute`) has a STATIC spi import that cannot
//! be satisfied by static composition — the ext↔shape spi cycle is not
//! wac-composable. So the host satisfies `sqlite:extension/spi` on the
//! RESIDENT provider's linker (see compose_provider::resident_wasm_component_invoke
//! + `impl bindings::sqlite::extension::spi::Host for ProviderSpiWrap`).
//!
//! This test proves it end to end:
//!   1. a `<ext>-provider.wasm` wrapping `eval` (the PLAIN scalar shape,
//!      which leaves `sqlite:extension/spi` imported) INSTANTIATES through
//!      the resident path — i.e. the host DOES satisfy the spi import
//!      (no "spi not found in the linker");
//!   2. dispatching the `eval` scalar re-enters the host's spi surface:
//!      `eval('SELECT 1')` -> the guest calls `spi.execute("SELECT 1")`
//!      -> the host runs it on the provider's connection -> "1".
//!
//! Skips gracefully when the fixture is absent (built out-of-band by
//! `wac plug --plug eval_extension.wasm scalar-shape.wasm`).

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

/// First `scalars[0].id` from the provider's describe manifest.
async fn first_scalar_id(provider: &ProviderHandle) -> u64 {
    let bytes = provider.invoke("describe", &[]).await.expect("describe");
    let m: Cbor = ciborium::de::from_reader(&*bytes).unwrap();
    let Cbor::Map(map) = &m else { panic!("manifest not a map") };
    let entries = map
        .iter()
        .find(|(k, _)| matches!(k, Cbor::Text(s) if s == "scalars"))
        .map(|(_, v)| v)
        .expect("no scalars tier");
    let Cbor::Array(a) = entries else { panic!("scalars not array") };
    let Cbor::Map(fm) = &a[0] else { panic!("entry not map") };
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

/// #220: a resident provider wrapping an spi-importing ext INSTANTIATES —
/// the host satisfies `sqlite:extension/spi` on the resident linker.
#[tokio::test(flavor = "multi_thread")]
async fn resident_provider_satisfies_spi_import() {
    let Some(path) = provider_path("eval-provider.wasm") else {
        eprintln!("skip: eval-provider.wasm absent (wac plug eval into the scalar shape)");
        return;
    };
    let host = Host::new().unwrap();
    // A resident provider (the spi surface is a resident-only concern).
    let provider = ProviderHandle::new_resident_wasm_component(
        host.engine().clone(),
        path,
        None,
        String::new(),
        None,
        None,
        None,
        false,
    )
    .expect("compile eval-provider");
    assert!(provider.is_resident());

    // `describe` forces the resident store to instantiate. If the host did
    // NOT satisfy the ext's `sqlite:extension/spi` import, instantiation
    // fails here with "spi ... not found in the linker".
    let bytes = provider
        .invoke("describe", &[])
        .await
        .expect("eval-provider must instantiate — the host satisfies the spi import");
    assert!(!bytes.is_empty(), "describe returned a manifest");
    eprintln!("[eval] resident provider instantiated; spi import satisfied by the host");
}

/// #220: dispatching the `eval` scalar re-enters the host spi surface.
/// `eval('SELECT 1')` calls `spi.execute("SELECT 1")` -> "1".
#[tokio::test(flavor = "multi_thread")]
async fn eval_scalar_reenters_spi_execute() {
    use sqlink_host::bindings::sqlite::extension::types::SqlValue;

    let Some(path) = provider_path("eval-provider.wasm") else {
        eprintln!("skip: eval-provider.wasm absent");
        return;
    };
    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_resident_wasm_component(
        host.engine().clone(),
        path,
        None,
        String::new(),
        None,
        None,
        None,
        false,
    )
    .expect("compile eval-provider");
    let func_id = first_scalar_id(&provider).await;

    host.load_extension_as_provider("eval", provider)
        .await
        .expect("provider-back eval (resident)");

    // eval('SELECT 1') -> guest calls spi.execute("SELECT 1") -> "1".
    let out = host
        .dispatch_scalar("eval", func_id, vec![SqlValue::Text("SELECT 1".into())])
        .await
        .expect("dispatch_scalar plumbing");
    match out {
        Ok(v) => {
            eprintln!("[eval] eval('SELECT 1') -> {v:?}");
            let got = match v {
                SqlValue::Text(s) => s,
                SqlValue::Integer(i) => i.to_string(),
                other => panic!("unexpected eval result: {other:?}"),
            };
            assert_eq!(got.trim(), "1", "eval('SELECT 1') must reenter spi.execute and return 1");
        }
        Err(e) => panic!("eval scalar errored (spi reentry failed): {e}"),
    }
}
