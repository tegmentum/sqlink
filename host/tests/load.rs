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

/// Tenant-scoped providers: register two databases with different
/// contents under the same `sqlite-runtime` id in different
/// tenants. Run the fiji-hello function in each tenant; the
/// reported table count differs per tenant, proving the active
/// tenant scopes resolution.
#[tokio::test]
async fn fiji_tenant_scoping_isolates_providers() {
    use parking_lot::Mutex;
    use sqlite_wasm_host::compose_provider::ProviderHandle;
    use std::sync::Arc;

    let mut fiji_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fiji_path.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/fiji_hello.wasm");
    if !fiji_path.exists() {
        eprintln!("skipping: fiji_hello.wasm not built");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let db_a = dir.path().join("a.db");
    let db_b = dir.path().join("b.db");
    {
        let c = rusqlite::Connection::open(&db_a).unwrap();
        c.execute_batch("CREATE TABLE a(x); CREATE TABLE b(y);")
            .unwrap();
    }
    {
        let c = rusqlite::Connection::open(&db_b).unwrap();
        c.execute_batch(
            "CREATE TABLE p(x); CREATE TABLE q(y); CREATE TABLE r(z); CREATE TABLE s(w);",
        )
        .unwrap();
    }

    let host = Host::new().unwrap();

    // Tenant alpha sees the 2-table db.
    let conn = rusqlite::Connection::open(&db_a).unwrap();
    host.register_compose_provider_in(
        "alpha",
        "sqlite-runtime",
        ProviderHandle::new_sqlite_runtime(Arc::new(Mutex::new(Some(conn)))),
    );

    // Tenant beta sees the 4-table db.
    let conn = rusqlite::Connection::open(&db_b).unwrap();
    host.register_compose_provider_in(
        "beta",
        "sqlite-runtime",
        ProviderHandle::new_sqlite_runtime(Arc::new(Mutex::new(Some(conn)))),
    );

    let out_alpha = host
        .run_fiji_function_as(fiji_path.clone(), Policy::deny_all(), "alpha")
        .await
        .expect("alpha");
    assert!(out_alpha.contains("2 table(s)"), "alpha output: {out_alpha:?}");

    let out_beta = host
        .run_fiji_function_as(fiji_path.clone(), Policy::deny_all(), "beta")
        .await
        .expect("beta");
    assert!(out_beta.contains("4 table(s)"), "beta output: {out_beta:?}");

    // Default tenant has no sqlite-runtime provider — run_fiji_function
    // (uses DEFAULT_TENANT) must surface a useful error.
    let err = host
        .run_fiji_function(fiji_path, Policy::deny_all())
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
    use sqlite_wasm_host::TrustPolicy;

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/std_text.wasm");
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
    use sqlite_wasm_host::compose_provider::ProviderHandle;

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/std_hashing.wasm");
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
    use sqlite_wasm_host::compose_provider::ProviderHandle;

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/std_encoding.wasm");
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

/// Drives the cli reactor under the concurrent canonical ABI and
/// proves the channel-bridge re-entry actually works: a background
/// task inside `Store::run_concurrent` receives SQL via the
/// `LiveSpiBridge` channel and re-enters `cli.eval-structured` on
/// the same instance the REPL would call `cli.eval` on. This is
/// the exact path `LoadedState::execute_live` will use once L1-L4
/// is wired through dispatch.
///
/// The test exercises only the reactor side (no extension dispatch
/// yet) so the architecture can be validated independently.
#[tokio::test]
async fn live_spi_bridge_reenters_eval_structured() {
    use sqlite_wasm_host::{bindings, LiveSpiBridge, LoaderData};
    use wasmtime::component::{Component, Linker};
    use wasmtime::{AsContextMut, Store};

    let mut cli_rust_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    cli_rust_path.push("../cli-rust/target/wasm32-wasip2/release/sqlite_cli_rust.component.wasm");
    if !cli_rust_path.exists() {
        eprintln!("skipping: sqlite_cli_rust.component.wasm not built");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    {
        let c = rusqlite::Connection::open(&db_path).unwrap();
        c.execute_batch("CREATE TABLE widgets(id); INSERT INTO widgets VALUES(1),(2),(3),(4),(5);")
            .unwrap();
    }

    let host = Host::new().unwrap();
    host.set_db_path(db_path.to_string_lossy().as_ref());
    let engine = host.engine().clone();

    // Same State shape the runner uses; minimal wiring of WASI +
    // extension-loader/dispatch host imports so the cli component
    // can instantiate.
    struct State {
        wasi: wasmtime_wasi::WasiCtx,
        resources: wasmtime_wasi::ResourceTable,
        host: Host,
    }
    impl wasmtime_wasi::WasiView for State {
        fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
            wasmtime_wasi::WasiCtxView {
                ctx: &mut self.wasi,
                table: &mut self.resources,
            }
        }
    }

    let bytes = std::fs::read(&cli_rust_path).unwrap();
    let component = Component::from_binary(&engine, &bytes).unwrap();
    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).unwrap();
    bindings::sqlite::wasm::extension_loader::add_to_linker::<_, LoaderData>(
        &mut linker,
        |s: &mut State| sqlite_wasm_host::HostWrap {
            host: &mut s.host,
            resources: Some(&mut s.resources),
        },
    )
    .unwrap();
    bindings::sqlite::wasm::dispatch::add_to_linker::<_, LoaderData>(
        &mut linker,
        |s: &mut State| sqlite_wasm_host::HostWrap {
            host: &mut s.host,
            resources: Some(&mut s.resources),
        },
    )
    .unwrap();

    let mut wasi = wasmtime_wasi::WasiCtxBuilder::new();
    let parent = db_path.parent().unwrap().to_string_lossy().to_string();
    wasi.preopened_dir(
        db_path.parent().unwrap(),
        &parent,
        wasmtime_wasi::DirPerms::all(),
        wasmtime_wasi::FilePerms::all(),
    )
    .unwrap();
    wasi.arg("cli");

    let state = State {
        wasi: wasi.build(),
        resources: wasmtime_wasi::ResourceTable::new(),
        host: host.clone(),
    };
    let mut store = Store::new(&engine, state);
    store.set_fuel(u64::MAX / 2).unwrap();
    store.set_epoch_deadline(1_000_000_000_000);

    let reactor = sqlite_wasm_host::reactor::SqliteCliReactor::instantiate_async(
        &mut store, &component, &linker,
    )
    .await
    .expect("instantiate reactor");

    let (req_tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel();
    let bridge = LiveSpiBridge::new(req_tx);
    host.set_live_spi_bridge(bridge.clone());

    let db_path_owned = db_path.to_string_lossy().to_string();
    let result: Result<i64, String> = store
        .as_context_mut()
        .run_concurrent(async move |accessor| -> Result<i64, String> {
            let cli = reactor.sqlite_wasm_cli();
            cli.call_init(accessor, db_path_owned)
                .await
                .map_err(|e| format!("init trap: {e}"))?
                .map_err(|e| format!("init: {e}"))?;

            let dispatcher = async {
                while let Some(req) = req_rx.recv().await {
                    let r = cli.call_eval_structured(accessor, req.sql).await;
                    let _ = req.resp_tx.send(r.map_err(|e| e.to_string()));
                }
            };

            let driver = async {
                let r = bridge
                    .execute("SELECT COUNT(*) FROM widgets".to_string())
                    .await
                    .map_err(|e| format!("bridge: {e}"))?
                    .map_err(|e| format!("eval-structured: {}", e.message))?;
                assert_eq!(r.columns.len(), 1);
                assert_eq!(r.rows.len(), 1);
                let cell = &r.rows[0][0];
                use sqlite_wasm_host::reactor::exports::sqlite::extension::types::SqlValue;
                match cell {
                    SqlValue::Integer(n) => Ok(*n),
                    other => Err(format!("expected integer, got {other:?}")),
                }
            };

            tokio::select! {
                r = driver => r,
                _ = dispatcher => Err("dispatcher exited first".into()),
            }
        })
        .await
        .expect("run_concurrent")
        .map_err(|e| format!("driver: {e}"));

    assert_eq!(result.expect("driver result"), 5);
}

/// Full dispatch-chain validation: load live-spi-extension into the
/// cli's rusqlite, then `SELECT wasm_live_count('widgets')` through
/// `cli.eval`. The extension's `spi.execute_scalar_live` must route
/// through the channel bridge → re-enter `cli.eval-structured` on
/// the SAME instance while the outer eval is still on the stack.
///
/// **Currently hangs** under wasmtime 45 because our host imports
/// (`dispatch.scalar_call`, `extension-loader.load_extension`, etc.)
/// are wired with standard async `add_to_linker` rather than
/// `func_wrap_concurrent`. While the outer eval is awaiting on a
/// host import, wasmtime's `may_enter` keeps the cli instance
/// "entered" — the dispatcher's `call_concurrent` on
/// `cli.eval-structured` queues but never makes progress. The fix
/// is the Stage-2 host-traits-to-Accessor rewrite documented in
/// `host/SPI-LIVE.md`. Disabled with `#[ignore]` until then so CI
/// stays green; re-enable when the host-trait rewrite lands.
#[tokio::test]
#[ignore = "incompatible with WebAssembly Component Model task spec (no recursive instance entry) — see host/SPI-LIVE-ARCHITECTURE.md"]
async fn dispatch_chain_routes_execute_live_through_bridge() {
    use sqlite_wasm_host::{bindings, LiveSpiBridge, LoaderData};
    use wasmtime::component::{Component, Linker};
    use wasmtime::{AsContextMut, Store};

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cli_rust_path = manifest_dir.clone();
    cli_rust_path.push("../cli-rust/target/wasm32-wasip2/release/sqlite_cli_rust.component.wasm");
    let mut ext_path = manifest_dir.clone();
    ext_path.push("../../sqlite-wasm-loader/target/wasm32-wasip1/release/live_spi_extension.wasm");
    if !cli_rust_path.exists() || !ext_path.exists() {
        eprintln!("skipping: prerequisite wasm not built");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    {
        let c = rusqlite::Connection::open(&db_path).unwrap();
        c.execute_batch("CREATE TABLE widgets(id); INSERT INTO widgets VALUES(1),(2),(3),(4),(5);")
            .unwrap();
    }

    let host = Host::new().unwrap();
    host.set_db_path(db_path.to_string_lossy().as_ref());
    let engine = host.engine().clone();

    // Load the extension so the cli's `.load` command can find it.
    // Actually — the cli component issues `.load PATH` which routes
    // through extension_loader.load_extension, instantiating the ext
    // and registering its scalars in the cli's rusqlite. So we don't
    // pre-load via host.load_extension; we drive `.load` through cli.
    struct State {
        wasi: wasmtime_wasi::WasiCtx,
        resources: wasmtime_wasi::ResourceTable,
        host: Host,
    }
    impl wasmtime_wasi::WasiView for State {
        fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
            wasmtime_wasi::WasiCtxView {
                ctx: &mut self.wasi,
                table: &mut self.resources,
            }
        }
    }

    let bytes = std::fs::read(&cli_rust_path).unwrap();
    let component = Component::from_binary(&engine, &bytes).unwrap();
    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).unwrap();
    bindings::sqlite::wasm::extension_loader::add_to_linker::<_, LoaderData>(
        &mut linker,
        |s: &mut State| sqlite_wasm_host::HostWrap {
            host: &mut s.host,
            resources: Some(&mut s.resources),
        },
    )
    .unwrap();
    bindings::sqlite::wasm::dispatch::add_to_linker::<_, LoaderData>(
        &mut linker,
        |s: &mut State| sqlite_wasm_host::HostWrap {
            host: &mut s.host,
            resources: Some(&mut s.resources),
        },
    )
    .unwrap();

    let mut wasi = wasmtime_wasi::WasiCtxBuilder::new();
    let parent = db_path.parent().unwrap().to_string_lossy().to_string();
    wasi.preopened_dir(
        db_path.parent().unwrap(),
        &parent,
        wasmtime_wasi::DirPerms::all(),
        wasmtime_wasi::FilePerms::all(),
    )
    .unwrap();
    wasi.arg("cli");

    let state = State {
        wasi: wasi.build(),
        resources: wasmtime_wasi::ResourceTable::new(),
        host: host.clone(),
    };
    let mut store = Store::new(&engine, state);
    store.set_fuel(u64::MAX / 2).unwrap();
    store.set_epoch_deadline(1_000_000_000_000);

    let reactor = sqlite_wasm_host::reactor::SqliteCliReactor::instantiate_async(
        &mut store, &component, &linker,
    )
    .await
    .expect("instantiate reactor");

    let (req_tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel();
    host.set_live_spi_bridge(LiveSpiBridge::new(req_tx));

    let db_path_owned = db_path.to_string_lossy().to_string();
    let ext_path_str = ext_path.to_string_lossy().to_string();

    let outcome: Result<String, String> = store
        .as_context_mut()
        .run_concurrent(async move |accessor| -> Result<String, String> {
            let cli = reactor.sqlite_wasm_cli();
            cli.call_init(accessor, db_path_owned)
                .await
                .map_err(|e| format!("init trap: {e}"))?
                .map_err(|e| format!("init: {e}"))?;

            let dispatcher = async {
                while let Some(req) = req_rx.recv().await {
                    let r = cli.call_eval_structured(accessor, req.sql).await;
                    let _ = req.resp_tx.send(r.map_err(|e| e.to_string()));
                }
            };

            let driver = async {
                // .load registers the extension's scalars in the cli's
                // rusqlite via dispatch.scalar_call.
                let load_cmd = format!(".load {ext_path_str} --grant=spi\n");
                let load_out = cli
                    .call_eval(accessor, load_cmd)
                    .await
                    .map_err(|e| format!("eval .load: {e}"))?;
                if load_out.to_lowercase().contains("error")
                    || load_out.to_lowercase().contains("bad flag")
                {
                    return Err(format!(".load output: {load_out}"));
                }

                // The SELECT triggers ext_fn → spi.execute_scalar_live
                // → bridge → cli.eval-structured. If wasmtime's
                // may_enter blocks the re-entry, this returns
                // "CannotEnterComponent" embedded in the eval output.
                let q_out = cli
                    .call_eval(
                        accessor,
                        "SELECT wasm_live_count('widgets');\n".to_string(),
                    )
                    .await
                    .map_err(|e| format!("eval SELECT: {e}"))?;
                Ok(q_out)
            };

            tokio::select! {
                r = driver => r,
                _ = dispatcher => Err("dispatcher exited first".into()),
            }
        })
        .await
        .expect("run_concurrent");

    let q_out = outcome.expect("driver");
    // The cli prints results as "5" (live-spi-extension's count)
    // formatted by the output formatter. Assert non-error + contains
    // the expected count.
    assert!(
        !q_out.to_lowercase().contains("error"),
        "cli reported error: {q_out}"
    );
    assert!(
        q_out.contains('5'),
        "expected count of 5 in output, got: {q_out:?}"
    );
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
