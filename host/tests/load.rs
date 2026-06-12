//! End-to-end test for the host's extension-loader path.
//!
//! Exercises Host::load_extension on a real wasm component
//! (sqlite-wasm-loader's test_extension.wasm) to validate that:
//!   - the wasmtime engine compiles the component
//!   - load_extension instantiates it against the canonical
//!     `sqlite:extension/minimal` world
//!   - describe() returns a manifest the host can read
//!   - the registry retains the loaded ext under its manifest name
//!   - is_loaded / list / unload work
//!
//! The wasm-demo extension in sqlite-wasm/extensions/wasm-demo/
//! exports per-slot interfaces (sqlite:wasm/demo-slot etc.) for
//! static composition, not the canonical sqlite:extension/metadata.
//! It can't be Host::load_extension-loaded because the host can't
//! call describe() on it. For that path, use a canonical-world
//! extension like sqlite-wasm-loader's test_extension.
//!
//! Tests silently skip if the wasm isn't built so the suite stays
//! green in environments without the wasm toolchain.

use std::path::PathBuf;

use sqlite_wasm_host::{Capability, Host, Policy};

/// Path to a canonical-world wasm extension. Uses sqlite-wasm-loader's
/// test_extension.wasm because it's already built against the
/// canonical sqlite:extension/minimal world and is the same binary
/// validated by the loader's integration tests.
fn canonical_ext_path() -> Option<PathBuf> {
    let candidates = [
        "../../sqlite-wasm-loader/target/wasm32-wasip1/release/test_extension.wasm",
        "../sqlite-wasm-loader/target/wasm32-wasip1/release/test_extension.wasm",
    ];
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for c in candidates {
        let p = manifest_dir.join(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[tokio::test]
async fn loads_and_unloads_an_extension() {
    let Some(path) = canonical_ext_path() else {
        eprintln!("skipping: test_extension.wasm not found (build sqlite-wasm-loader's test-extension)");
        return;
    };

    let host = Host::new().expect("engine");
    assert!(host.list().is_empty(), "registry starts empty");

    let policy = Policy::deny_all().with_grants([Capability::Text]);
    let name = host.load_extension(path, policy).await.expect("load");

    // Manifest's `name` field becomes the registry key. test_extension
    // declares "test-extension".
    assert_eq!(name, "test-extension");
    assert!(host.is_loaded(&name));
    assert_eq!(host.list(), vec!["test-extension".to_string()]);

    host.unload(&name).expect("unload");
    assert!(!host.is_loaded(&name));
    assert!(host.list().is_empty());
}

#[tokio::test]
async fn double_unload_errors() {
    let host = Host::new().expect("engine");
    let err = host.unload("never-loaded").err().expect("must error");
    assert!(err.to_string().contains("never-loaded"));
}
