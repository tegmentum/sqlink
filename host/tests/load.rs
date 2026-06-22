//! End-to-end test for the host's extension-loader path.
//!
//! Exercises Host::load_extension on a real wasm component
//! (sqlink-loader's test_extension.wasm) to validate that:
//!   - the wasmtime engine compiles the component
//!   - load_extension instantiates it against the canonical
//!     `sqlite:extension/minimal` world
//!   - describe() returns a manifest the host can read
//!   - the registry retains the loaded ext under its manifest name
//!   - is_loaded / list / unload work
//!
//! The wasm-demo extension in sqlink/extensions/wasm-demo/
//! exports per-slot interfaces (sqlite:wasm/demo-slot etc.) for
//! static composition, not the canonical sqlite:extension/metadata.
//! It can't be Host::load_extension-loaded because the host can't
//! call describe() on it. For that path, use a canonical-world
//! extension like sqlink-loader's test_extension.
//!
//! Tests silently skip if the wasm isn't built so the suite stays
//! green in environments without the wasm toolchain.

use std::path::{Path, PathBuf};

use sqlink_host::{Capability, Host, Policy};
use sqlink_core::db;

fn open_db(path: &Path) -> db::Connection {
    db::Connection::open(
        path.to_str().expect("non-UTF8 path"),
        db::OpenFlags::DEFAULT,
    )
    .expect("open db")
}

/// Path to a canonical-world wasm extension. Uses sqlink-loader's
/// test_extension.wasm because it's already built against the
/// canonical sqlite:extension/minimal world and is the same binary
/// validated by the loader's integration tests.
fn canonical_ext_path() -> Option<PathBuf> {
    let candidates = [
        "../../sqlink-loader/target/wasm32-wasip1/release/test_extension.wasm",
        "../sqlink-loader/target/wasm32-wasip1/release/test_extension.wasm",
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
            "skipping: test_extension.wasm not found (build sqlink-loader's test-extension)"
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
async fn run_resolves_sqlite_runtime() {
    use parking_lot::Mutex;
    use sqlink_host::compose_provider::ProviderHandle;
    use std::sync::Arc;

    let mut wasm_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    wasm_path.push("../../sqlink-loader/target/wasm32-wasip1/release/runnable_hello.wasm");
    if !wasm_path.exists() {
        eprintln!("skipping: runnable_hello.wasm not built");
        return;
    }

    // Open a temp file-backed db; populate with N tables; expect
    // the wasm component to report N.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    {
        let c = open_db(&db_path);
        c.execute_batch("CREATE TABLE a(x); CREATE TABLE b(y); CREATE TABLE c(z);")
            .unwrap();
    }

    let host = Host::new().unwrap();

    // Register sqlite-runtime against the same file.
    let conn = open_db(&db_path);
    let conn_arc = Arc::new(Mutex::new(Some(conn)));
    host.register_compose_provider(
        "sqlite-runtime",
        ProviderHandle::new_sqlite_runtime(conn_arc),
    );

    let output = host
        .run_wasm(wasm_path, Policy::deny_all())
        .await
        .expect("wasm run");

    assert!(output.contains("3 table(s)"), "got: {output:?}");
}

#[tokio::test]
async fn wasm_component_provider_handles_invoke() {
    use ciborium::value::Value as CborValue;
    use sqlink_host::compose_provider::ProviderHandle;

    let mut std_text = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std_text.push("../../sqlink-loader/target/wasm32-wasip1/release/std_text.wasm");
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

/// Tenant-scoped providers: register two databases with different
/// contents under the same `sqlite-runtime` id in different
/// tenants. Run runnable-hello in each tenant; the reported
/// table count differs per tenant, proving the active tenant
/// scopes resolution.
#[tokio::test]
async fn run_tenant_scoping_isolates_providers() {
    use parking_lot::Mutex;
    use sqlink_host::compose_provider::ProviderHandle;
    use std::sync::Arc;

    let mut wasm_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    wasm_path.push("../../sqlink-loader/target/wasm32-wasip1/release/runnable_hello.wasm");
    if !wasm_path.exists() {
        eprintln!("skipping: runnable_hello.wasm not built");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let db_a = dir.path().join("a.db");
    let db_b = dir.path().join("b.db");
    {
        let c = open_db(&db_a);
        c.execute_batch("CREATE TABLE a(x); CREATE TABLE b(y);")
            .unwrap();
    }
    {
        let c = open_db(&db_b);
        c.execute_batch(
            "CREATE TABLE p(x); CREATE TABLE q(y); CREATE TABLE r(z); CREATE TABLE s(w);",
        )
        .unwrap();
    }

    let host = Host::new().unwrap();

    // Tenant alpha sees the 2-table db.
    let conn = open_db(&db_a);
    host.register_compose_provider_in(
        "alpha",
        "sqlite-runtime",
        ProviderHandle::new_sqlite_runtime(Arc::new(Mutex::new(Some(conn)))),
    );

    // Tenant beta sees the 4-table db.
    let conn = open_db(&db_b);
    host.register_compose_provider_in(
        "beta",
        "sqlite-runtime",
        ProviderHandle::new_sqlite_runtime(Arc::new(Mutex::new(Some(conn)))),
    );

    let out_alpha = host
        .run_wasm_as(wasm_path.clone(), Policy::deny_all(), "alpha")
        .await
        .expect("alpha");
    assert!(out_alpha.contains("2 table(s)"), "alpha output: {out_alpha:?}");

    let out_beta = host
        .run_wasm_as(wasm_path.clone(), Policy::deny_all(), "beta")
        .await
        .expect("beta");
    assert!(out_beta.contains("4 table(s)"), "beta output: {out_beta:?}");

    // Default tenant has no sqlite-runtime provider — run_wasm
    // (uses DEFAULT_TENANT) must surface a useful error.
    let err = host
        .run_wasm(wasm_path, Policy::deny_all())
        .await
        .expect_err("default tenant has no provider");
    assert!(
        err.to_string().contains("default"),
        "expected tenant in err, got: {err}"
    );

    // list_compose_providers exposes per-tenant rows.
    let listed = host.list_compose_providers();
    assert!(listed.iter().any(|(t, id, _)| t == "alpha" && id == "sqlite-runtime"));
    assert!(listed.iter().any(|(t, id, _)| t == "beta" && id == "sqlite-runtime"));
}

/// Provider trust policy gates wasm-component registration by
/// blake3 digest. AllowAll (default) accepts anything; DenyAll
/// rejects everything; DigestAllowlist accepts only whitelisted
/// digests.
#[tokio::test]
async fn trust_policy_gates_provider_registration() {
    use sqlink_host::TrustPolicy;

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../sqlink-loader/target/wasm32-wasip1/release/std_text.wasm");
    if !path.exists() {
        eprintln!("skipping: std_text.wasm not built");
        return;
    }
    let bytes = std::fs::read(&path).unwrap();
    let digest = blake3::hash(&bytes).to_hex().to_string();

    // Default AllowAll: registration succeeds.
    let host = Host::new().unwrap();
    host.register_wasm_provider("std-text", path.clone())
        .expect("AllowAll allows");

    // DenyAll: registration fails.
    let host = Host::new().unwrap();
    host.set_trust_policy(TrustPolicy::DenyAll);
    let err = host
        .register_wasm_provider("std-text", path.clone())
        .expect_err("DenyAll rejects");
    assert!(err.to_string().contains("DenyAll"), "got: {err}");

    // DigestAllowlist with the right digest: succeeds.
    let host = Host::new().unwrap();
    let mut set = std::collections::HashSet::new();
    set.insert(digest.clone());
    host.set_trust_policy(TrustPolicy::DigestAllowlist(set));
    host.register_wasm_provider("std-text", path.clone())
        .expect("matching digest");

    // DigestAllowlist with the wrong digest: fails with a message
    // mentioning the actual digest so operators can update their
    // allowlist.
    let host = Host::new().unwrap();
    let mut set = std::collections::HashSet::new();
    set.insert("0".repeat(64));
    host.set_trust_policy(TrustPolicy::DigestAllowlist(set));
    let err = host
        .register_wasm_provider("std-text", path)
        .expect_err("wrong digest");
    assert!(
        err.to_string().contains(&digest),
        "expected digest in err, got: {err}"
    );
}

/// std-hashing provider: pin one known digest for each method.
/// Same wasm-component invoke shape as wasm_component_provider_handles_invoke.
#[tokio::test]
async fn std_hashing_provider() {
    use ciborium::value::Value as CborValue;
    use sqlink_host::compose_provider::ProviderHandle;

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../sqlink-loader/target/wasm32-wasip1/release/std_hashing.wasm");
    if !path.exists() {
        eprintln!("skipping: std_hashing.wasm not built");
        return;
    }
    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_wasm_component(host.engine().clone(), path)
        .expect("compile std-hashing");

    let req = {
        let mut buf = Vec::new();
        let map = CborValue::Map(vec![(
            CborValue::Text("text".into()),
            CborValue::Text("abc".into()),
        )]);
        ciborium::ser::into_writer(&map, &mut buf).unwrap();
        buf
    };

    // Known digests of "abc":
    //   sha256: ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    //   sha512: ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f
    //   md5:    900150983cd24fb0d6963f7d28e17f72
    //   blake3: 6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85
    for (method, expected) in [
        (
            "sha256",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "sha512",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        ),
        ("md5", "900150983cd24fb0d6963f7d28e17f72"),
        (
            "blake3",
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85",
        ),
    ] {
        let resp = provider.invoke(method, &req).await.expect("invoke");
        let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
        match v {
            CborValue::Text(s) => assert_eq!(s, expected, "{method} digest mismatch"),
            other => panic!("expected text for {method}, got {other:?}"),
        }
    }
}

/// std-encoding provider: round-trip + decode-error.
#[tokio::test]
async fn std_encoding_provider() {
    use ciborium::value::Value as CborValue;
    use sqlink_host::compose_provider::ProviderHandle;

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../sqlink-loader/target/wasm32-wasip1/release/std_encoding.wasm");
    if !path.exists() {
        eprintln!("skipping: std_encoding.wasm not built");
        return;
    }
    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_wasm_component(host.engine().clone(), path)
        .expect("compile std-encoding");

    let encode_req = |bytes: Vec<u8>| -> Vec<u8> {
        let mut buf = Vec::new();
        let map = CborValue::Map(vec![(CborValue::Text("data".into()), CborValue::Bytes(bytes))]);
        ciborium::ser::into_writer(&map, &mut buf).unwrap();
        buf
    };
    let decode_req = |s: &str| -> Vec<u8> {
        let mut buf = Vec::new();
        let map = CborValue::Map(vec![(
            CborValue::Text("text".into()),
            CborValue::Text(s.into()),
        )]);
        ciborium::ser::into_writer(&map, &mut buf).unwrap();
        buf
    };

    // base64
    let resp = provider
        .invoke("base64-encode", &encode_req(b"hello".to_vec()))
        .await
        .expect("b64 encode");
    let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
    match v {
        CborValue::Text(s) => assert_eq!(s, "aGVsbG8="),
        other => panic!("expected text, got {other:?}"),
    }
    let resp = provider
        .invoke("base64-decode", &decode_req("aGVsbG8="))
        .await
        .expect("b64 decode");
    let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
    match v {
        CborValue::Bytes(b) => assert_eq!(b, b"hello"),
        other => panic!("expected bytes, got {other:?}"),
    }

    // hex
    let resp = provider
        .invoke("hex-encode", &encode_req(vec![0xde, 0xad, 0xbe, 0xef]))
        .await
        .expect("hex encode");
    let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
    match v {
        CborValue::Text(s) => assert_eq!(s, "deadbeef"),
        other => panic!("expected text, got {other:?}"),
    }

    // url-encode preserves alphanumeric, percent-encodes the rest
    let resp = provider
        .invoke("url-encode", &encode_req(b"hello world!".to_vec()))
        .await
        .expect("url encode");
    let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
    match v {
        CborValue::Text(s) => assert_eq!(s, "hello%20world%21"),
        other => panic!("expected text, got {other:?}"),
    }

    // decode-error path: invalid base64 surfaces a structured Err.
    let err = provider
        .invoke("base64-decode", &decode_req("not!base64!!"))
        .await
        .expect_err("invalid base64");
    assert!(err.contains("base64"), "got: {err}");
}

#[tokio::test]
async fn run_composes_sqlite_runtime_and_std_text() {
    use parking_lot::Mutex;
    use sqlink_host::compose_provider::ProviderHandle;
    use std::sync::Arc;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut wasm_path = manifest_dir.clone();
    wasm_path.push("../../sqlink-loader/target/wasm32-wasip1/release/runnable_text_demo.wasm");
    let mut std_text = manifest_dir.clone();
    std_text.push("../../sqlink-loader/target/wasm32-wasip1/release/std_text.wasm");
    if !wasm_path.exists() || !std_text.exists() {
        eprintln!("skipping: runnable_text_demo.wasm or std_text.wasm not built");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    {
        let c = open_db(&db_path);
        c.execute_batch("CREATE TABLE widgets(x);").unwrap();
    }

    let host = Host::new().unwrap();
    let conn = open_db(&db_path);
    host.register_compose_provider(
        "sqlite-runtime",
        ProviderHandle::new_sqlite_runtime(Arc::new(Mutex::new(Some(conn)))),
    );
    host.register_wasm_provider("std-text", std_text)
        .expect("register std-text");

    let output = host
        .run_wasm(wasm_path, Policy::deny_all())
        .await
        .expect("wasm run");
    assert!(output.contains("WIDGETS"), "got: {output:?}");
}
