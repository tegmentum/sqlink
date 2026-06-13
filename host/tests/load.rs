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
        eprintln!(
            "skipping: test_extension.wasm not found (build sqlite-wasm-loader's test-extension)"
        );
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
    let err = host.unload("never-loaded").expect_err("must error");
    assert!(err.to_string().contains("never-loaded"));
}

#[tokio::test]
async fn fiji_function_resolves_sqlite_runtime() {
    use parking_lot::Mutex;
    use sqlite_wasm_host::compose_provider::ProviderHandle;
    use std::sync::Arc;

    let mut fiji_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fiji_path.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/fiji_hello.wasm");
    if !fiji_path.exists() {
        eprintln!("skipping: fiji_hello.wasm not built");
        return;
    }

    // Open a temp file-backed db; populate with N tables; expect
    // the fiji function to report N.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    {
        let c = rusqlite::Connection::open(&db_path).unwrap();
        c.execute_batch("CREATE TABLE a(x); CREATE TABLE b(y); CREATE TABLE c(z);")
            .unwrap();
    }

    let host = Host::new().unwrap();

    // Register sqlite-runtime against the same file.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let conn_arc = Arc::new(Mutex::new(Some(conn)));
    host.register_compose_provider(
        "sqlite-runtime",
        ProviderHandle::new_sqlite_runtime(conn_arc),
    );

    let output = host
        .run_fiji_function(fiji_path, Policy::deny_all())
        .await
        .expect("fiji run");

    assert!(output.contains("3 table(s)"), "got: {output:?}");
}

#[tokio::test]
async fn wasm_component_provider_handles_invoke() {
    use ciborium::value::Value as CborValue;
    use sqlite_wasm_host::compose_provider::ProviderHandle;

    let mut std_text = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std_text.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/std_text.wasm");
    if !std_text.exists() {
        eprintln!("skipping: std_text.wasm not built");
        return;
    }

    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_wasm_component(host.engine().clone(), std_text)
        .expect("compile std-text");

    // upper("hello") -> "HELLO"
    let req = {
        let mut buf = Vec::new();
        let map = CborValue::Map(vec![(
            CborValue::Text("text".into()),
            CborValue::Text("hello".into()),
        )]);
        ciborium::ser::into_writer(&map, &mut buf).unwrap();
        buf
    };
    let resp = provider.invoke("upper", &req).await.expect("invoke");
    let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
    match v {
        CborValue::Text(s) => assert_eq!(s, "HELLO"),
        other => panic!("expected text, got {other:?}"),
    }

    // len("hello") -> 5
    let resp = provider.invoke("len", &req).await.expect("invoke len");
    let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
    match v {
        CborValue::Integer(i) => {
            let n: i128 = i.into();
            assert_eq!(n, 5);
        }
        other => panic!("expected int, got {other:?}"),
    }

    // unknown method -> Err
    let err = provider.invoke("bogus", &req).await.expect_err("unknown");
    assert!(err.contains("unknown method"), "got: {err}");
}

/// live-spi-extension exports two scalars: wasm_committed_count and
/// wasm_live_count. Both end-to-end through the host's
/// `spi.execute_scalar` / `spi.execute_scalar_live` imports.
///
/// In v1 the live path opens a fresh rusqlite::Connection per call
/// while the committed path uses a pooled connection — both see the
/// committed snapshot, so the scalar results agree. The day true
/// re-entry (cli.eval-structured) lands, only the live path will
/// surface outer-transaction uncommitted writes. The test wires
/// both today so the contract is exercised end-to-end and any
/// regression in the SPI imports is caught.
///
/// See host/SPI-LIVE.md for the upstream status of true live
/// semantics under wasmtime's concurrent canonical ABI.
#[tokio::test]
async fn live_spi_extension_invokes_both_scalars() {
    use sqlite_wasm_host::bindings::sqlite::extension::types::SqlValue;

    let mut ext_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    ext_path.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/live_spi_extension.wasm");
    if !ext_path.exists() {
        eprintln!("skipping: live_spi_extension.wasm not built");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    {
        let c = rusqlite::Connection::open(&db_path).unwrap();
        c.execute_batch("CREATE TABLE widgets(id); INSERT INTO widgets VALUES(1),(2),(3),(4);")
            .unwrap();
    }

    let host = Host::new().unwrap();
    host.set_db_path(db_path.to_string_lossy().as_ref());

    let policy = Policy::deny_all().with_grants([Capability::Spi]);
    let name = host.load_extension(ext_path, policy).await.expect("load");
    assert_eq!(name, "live-spi-extension");

    // Both function ids per live-spi-extension/src/lib.rs:
    //   1 = wasm_live_count, 2 = wasm_committed_count
    let args = vec![SqlValue::Text("widgets".to_string())];

    let live = host
        .dispatch_scalar(&name, 1, args.clone())
        .await
        .expect("dispatch live")
        .expect("scalar ok");
    let committed = host
        .dispatch_scalar(&name, 2, args)
        .await
        .expect("dispatch committed")
        .expect("scalar ok");

    match (live, committed) {
        (SqlValue::Integer(l), SqlValue::Integer(c)) => {
            assert_eq!(l, 4, "live count");
            assert_eq!(c, 4, "committed count");
        }
        other => panic!("expected (Integer, Integer), got {other:?}"),
    }
}

#[tokio::test]
async fn fiji_composes_sqlite_runtime_and_std_text() {
    use parking_lot::Mutex;
    use sqlite_wasm_host::compose_provider::ProviderHandle;
    use std::sync::Arc;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut fiji = manifest_dir.clone();
    fiji.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/fiji_text_demo.wasm");
    let mut std_text = manifest_dir.clone();
    std_text.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/std_text.wasm");
    if !fiji.exists() || !std_text.exists() {
        eprintln!("skipping: fiji_text_demo.wasm or std_text.wasm not built");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    {
        let c = rusqlite::Connection::open(&db_path).unwrap();
        c.execute_batch("CREATE TABLE widgets(x);").unwrap();
    }

    let host = Host::new().unwrap();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    host.register_compose_provider(
        "sqlite-runtime",
        ProviderHandle::new_sqlite_runtime(Arc::new(Mutex::new(Some(conn)))),
    );
    host.register_wasm_provider("std-text", std_text)
        .expect("register std-text");

    let output = host
        .run_fiji_function(fiji, Policy::deny_all())
        .await
        .expect("fiji run");
    assert!(output.contains("WIDGETS"), "got: {output:?}");
}
