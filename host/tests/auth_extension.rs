//! T2.6 from PLAN-final-threads — acceptance test for the
//! authorizer dispatch path via a real loaded extension.
//!
//! The extension lives in `sqlite-wasm-loader/runtimes/wasmtime/
//! auth-extension/`. It targets the canonical `authorizing` world,
//! declares `has_authorizer: true`, and DENIES `CreateTable` +
//! `DropTable` while letting everything else through.
//!
//! What this test asserts about the path
//! `Host::load_extension → registry → Host::dispatch_authorize`:
//!
//!   1. `dispatch_authorize` instantiates the loaded extension
//!      against the `authorizing` world without errors.
//!   2. CreateTable + DropTable get back `AuthResult::Deny` (the
//!      extension's deny rule fires).
//!   3. Select (and other unrelated actions) get back
//!      `AuthResult::Ok` (the extension's catch-all `_ => Ok`).
//!
//! Together those three assertions exercise the same authorizer
//! dispatch path that `cli/src/lib.rs:822` wires
//! `conn.set_authorizer` against — which is the in-cli-rust side
//! of T2.4.
//!
//! Test skips if `auth_extension.wasm` isn't built in the sibling
//! `sqlite-wasm-loader` repo, matching the convention in
//! `host/tests/load.rs`.

use std::path::PathBuf;

use sqlink_host::bindings::sqlite::extension::types::{AuthAction, AuthResult};
use sqlink_host::{Capability, Host, Policy};

fn auth_ext_path() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        "../../sqlite-wasm-loader/target/wasm32-wasip1/release/auth_extension.wasm",
        "../sqlite-wasm-loader/target/wasm32-wasip1/release/auth_extension.wasm",
    ];
    for c in candidates {
        let p = manifest_dir.join(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[tokio::test]
async fn dispatches_authorize_through_loaded_extension() {
    let Some(path) = auth_ext_path() else {
        eprintln!("skipping: auth_extension.wasm not found (build sqlite-wasm-loader's auth-extension)");
        return;
    };

    let host = Host::new().expect("engine");

    // auth-extension declares no capabilities, but Capability::Text
    // gets granted defensively — load_extension's policy check
    // refuses the load if any declared capability isn't granted, so
    // a broader policy is the safer default for "I don't care about
    // gating here, I'm testing the dispatch path."
    let policy = Policy::deny_all().with_grants([Capability::Text]);
    let name = host.load_extension(path, policy).await.expect("load");
    assert_eq!(name, "auth-extension");

    // The extension's manifest declares has_authorizer = true, so
    // the registry entry retains the component as authorizing-world
    // instantiable. dispatch_authorize spins up a fresh Store +
    // instance per call (per the comment on dispatch_authorize).
    let create_result = host
        .dispatch_authorize(
            &name,
            AuthAction::CreateTable,
            Some("widgets".to_string()),
            None,
            Some("main".to_string()),
            None,
        )
        .await
        .expect("dispatch CreateTable");
    assert!(
        matches!(create_result, AuthResult::Deny),
        "auth-extension should DENY CreateTable; got {create_result:?}"
    );

    let drop_result = host
        .dispatch_authorize(
            &name,
            AuthAction::DropTable,
            Some("widgets".to_string()),
            None,
            Some("main".to_string()),
            None,
        )
        .await
        .expect("dispatch DropTable");
    assert!(
        matches!(drop_result, AuthResult::Deny),
        "auth-extension should DENY DropTable; got {drop_result:?}"
    );

    let select_result = host
        .dispatch_authorize(
            &name,
            AuthAction::Select,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("dispatch Select");
    assert!(
        matches!(select_result, AuthResult::Ok),
        "auth-extension should ALLOW Select; got {select_result:?}"
    );

    host.unload(&name).expect("unload");
}
