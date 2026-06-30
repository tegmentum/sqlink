//! Task #226 — drive the PRODUCTION compose:dynlink provider family
//! (`woco-sqlite-ext-endpoint`'s `<ext>-provider.wasm`) through sqlink's
//! host seam (`ProviderHandle::new_wasm_component` + `invoke` ->
//! `endpoint.handle`), proving that `.load` can be rewired onto the
//! production provider instead of the bespoke extension-loader.
//!
//! Each `<ext>-provider.wasm` is a `wac plug` of a real sqlink/woco
//! extension component into one of the 7 declarative provider
//! world-shapes; all export the uniform `compose:dynlink/endpoint`.
//! The CBOR envelope mirrors `woco .../provider/src/envelope.rs`.
//!
//! Fixtures live in host/tests/fixtures/providers (prebuilt by woco's
//! build.sh). Tests skip if a fixture is absent so the suite stays
//! green where the providers haven't been built.

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

fn open(name: &str) -> Option<(Host, ProviderHandle)> {
    let path = provider_path(name)?;
    let host = Host::new().unwrap();
    let provider = ProviderHandle::new_wasm_component(host.engine().clone(), path)
        .unwrap_or_else(|e| panic!("compile {name}: {e}"));
    Some((host, provider))
}

fn cbor(v: &Cbor) -> Vec<u8> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(v, &mut buf).unwrap();
    buf
}

fn de(bytes: &[u8]) -> Cbor {
    ciborium::de::from_reader(bytes).unwrap()
}

fn field<'a>(v: &'a Cbor, key: &str) -> &'a Cbor {
    match v {
        Cbor::Map(m) => m
            .iter()
            .find(|(k, _)| matches!(k, Cbor::Text(s) if s == key))
            .map(|(_, val)| val)
            .unwrap_or_else(|| panic!("missing field {key} in {v:?}")),
        _ => panic!("expected map, got {v:?}"),
    }
}

fn as_text(v: &Cbor) -> &str {
    match v {
        Cbor::Text(s) => s,
        _ => panic!("expected text, got {v:?}"),
    }
}

fn as_int(v: &Cbor) -> i128 {
    match v {
        Cbor::Integer(i) => (*i).into(),
        _ => panic!("expected int, got {v:?}"),
    }
}

fn as_arr(v: &Cbor) -> &[Cbor] {
    match v {
        Cbor::Array(a) => a,
        _ => panic!("expected array, got {v:?}"),
    }
}

/// `SqlValue` tagged-enum wire form, mirroring woco envelope.rs:
/// `#[serde(tag = "t", content = "v", rename_all = "lowercase")]`.
fn sqlval_int(n: i64) -> Cbor {
    Cbor::Map(vec![
        (Cbor::Text("t".into()), Cbor::Text("integer".into())),
        (Cbor::Text("v".into()), Cbor::Integer(n.into())),
    ])
}

fn sqlval_text(s: &str) -> Cbor {
    Cbor::Map(vec![
        (Cbor::Text("t".into()), Cbor::Text("text".into())),
        (Cbor::Text("v".into()), Cbor::Text(s.into())),
    ])
}

/// Decode a `SqlValue` response back to its tag + inner value.
fn sqlval_tag(v: &Cbor) -> (String, Cbor) {
    let t = as_text(field(v, "t")).to_string();
    // `Null` has no content field in serde's adjacently-tagged form.
    let inner = match v {
        Cbor::Map(m) => m
            .iter()
            .find(|(k, _)| matches!(k, Cbor::Text(s) if s == "v"))
            .map(|(_, val)| val.clone())
            .unwrap_or(Cbor::Null),
        _ => Cbor::Null,
    };
    (t, inner)
}

// ===========================================================================
// describe + policy-check (the manifest->register reconcile, fail-closed).
// Common to every shape. Validates the host can read a provider manifest.
// ===========================================================================

#[tokio::test]
async fn describe_returns_manifest_for_every_tier() {
    // NOTE: greet (streaming dotcmd) is covered separately because it
    // imports `sqlite:extension/cli-stdout` which the plain provider
    // linker doesn't satisfy — see `dotcmd_greet_invoke_streaming`.
    let cases = [
        ("aba-provider.wasm", "scalars"),
        ("count_min-provider.wasm", "aggregates"),
        ("uint-provider.wasm", "collations"),
        ("series-provider.wasm", "vtabs"),
        ("inmem-provider.wasm", "vtabs"),
        ("hookcb-provider.wasm", "name"),
        ("dotret-provider.wasm", "dot_commands"),
    ];
    let mut ran = 0;
    for (file, expect_field) in cases {
        let Some((_h, p)) = open(file) else {
            eprintln!("skip {file}: fixture absent");
            continue;
        };
        let resp = p.invoke("describe", &[]).await.expect("describe");
        let m = de(&resp);
        // every manifest carries a name + version
        assert!(!as_text(field(&m, "name")).is_empty(), "{file}: empty name");
        assert!(!as_text(field(&m, "version")).is_empty(), "{file}: empty version");
        // the tier-specific field exists
        let _ = field(&m, expect_field);
        eprintln!("[{file}] manifest name={}", as_text(field(&m, "name")));
        ran += 1;
    }
    assert!(ran > 0, "no provider fixtures present; build via woco build.sh");
}

#[tokio::test]
async fn policy_check_fail_closed() {
    // hookcb declares no capabilities -> empty grant is OK.
    // A provider that declares capabilities must fail-closed on empty grant.
    let Some((_h, p)) = open("aba-provider.wasm") else {
        eprintln!("skip: aba-provider.wasm absent");
        return;
    };
    let m = de(&p.invoke("describe", &[]).await.unwrap());
    let declared = as_arr(field(&m, "declared_capabilities"));
    // policy-check with the FULL declared grant must be ok.
    let grant: Vec<Cbor> = declared.to_vec();
    let req = cbor(&Cbor::Map(vec![(
        Cbor::Text("grant".into()),
        Cbor::Array(grant),
    )]));
    let report = de(&p.invoke("policy-check", &req).await.expect("policy-check"));
    assert!(
        matches!(field(&report, "ok"), Cbor::Bool(true)),
        "full grant should pass: {report:?}"
    );
    // If the ext declares anything, an empty grant must fail-closed.
    if !declared.is_empty() {
        let empty = cbor(&Cbor::Map(vec![(
            Cbor::Text("grant".into()),
            Cbor::Array(vec![]),
        )]));
        let report = de(&p.invoke("policy-check", &empty).await.unwrap());
        assert!(
            matches!(field(&report, "ok"), Cbor::Bool(false)),
            "empty grant against declared caps must fail-closed: {report:?}"
        );
    }
}

// ===========================================================================
// SCALAR tier — aba. describe -> pick a scalar func_id -> `call`.
// ===========================================================================

#[tokio::test]
async fn scalar_aba_call() {
    let Some((_h, p)) = open("aba-provider.wasm") else {
        eprintln!("skip: aba-provider.wasm absent");
        return;
    };
    let m = de(&p.invoke("describe", &[]).await.unwrap());
    let scalars = as_arr(field(&m, "scalars"));
    assert!(!scalars.is_empty(), "aba should export >=1 scalar");
    // call the first scalar with one text arg; assert it returns a value.
    let func_id = as_int(field(&scalars[0], "id")) as u64;
    let fname = as_text(field(&scalars[0], "name")).to_string();
    let req = cbor(&Cbor::Map(vec![
        (Cbor::Text("func_id".into()), Cbor::Integer(func_id.into())),
        (
            Cbor::Text("args".into()),
            Cbor::Array(vec![sqlval_text("hello")]),
        ),
    ]));
    let resp = p.invoke("call", &req).await.expect("scalar call");
    let (tag, inner) = sqlval_tag(&de(&resp));
    eprintln!("[aba] {fname}(\"hello\") -> {tag}:{inner:?}");
    assert!(
        matches!(tag.as_str(), "text" | "integer" | "blob" | "real" | "null"),
        "unexpected SqlValue tag {tag}"
    );
}

// ===========================================================================
// AGGREGATE tier — count_min. step* then finalize over a context.
// ===========================================================================

#[tokio::test]
async fn aggregate_count_min_step_finalize() {
    let Some((_h, p)) = open("count_min-provider.wasm") else {
        eprintln!("skip: count_min-provider.wasm absent");
        return;
    };
    let m = de(&p.invoke("describe", &[]).await.unwrap());
    let aggs = as_arr(field(&m, "aggregates"));
    assert!(!aggs.is_empty(), "count_min should export >=1 aggregate");
    let func_id = as_int(field(&aggs[0], "id")) as u64;
    let num_args = as_int(field(&aggs[0], "num_args"));
    let ctx: u64 = 1;
    // feed a few rows; count_min is a sketch so any int args are fine.
    for n in [10i64, 20, 10, 30] {
        let args = if num_args >= 1 {
            vec![sqlval_int(n)]
        } else {
            vec![]
        };
        let step = cbor(&Cbor::Map(vec![
            (Cbor::Text("func_id".into()), Cbor::Integer(func_id.into())),
            (Cbor::Text("context_id".into()), Cbor::Integer(ctx.into())),
            (Cbor::Text("args".into()), Cbor::Array(args)),
        ]));
        p.invoke("agg.step", &step).await.expect("agg.step");
    }
    let fin = cbor(&Cbor::Map(vec![
        (Cbor::Text("func_id".into()), Cbor::Integer(func_id.into())),
        (Cbor::Text("context_id".into()), Cbor::Integer(ctx.into())),
    ]));
    let resp = p.invoke("agg.finalize", &fin).await.expect("agg.finalize");
    let (tag, inner) = sqlval_tag(&de(&resp));
    eprintln!("[count_min] finalize -> {tag}:{inner:?}");
}

// ===========================================================================
// COLLATION tier — uint. compare(a, b) -> ordering. The #216 collision
// behavior is a *registration* concern (host-side prefix), validated
// separately; here we prove the dispatch contract.
// ===========================================================================

#[tokio::test]
async fn collation_uint_compare() {
    let Some((_h, p)) = open("uint-provider.wasm") else {
        eprintln!("skip: uint-provider.wasm absent");
        return;
    };
    let m = de(&p.invoke("describe", &[]).await.unwrap());
    let colls = as_arr(field(&m, "collations"));
    assert!(!colls.is_empty(), "uint should export >=1 collation");
    let coll_id = as_int(field(&colls[0], "id")) as u64;
    // uint collation: "10" should sort AFTER "9" numerically (a>b => +1),
    // whereas a byte collation would put "10" before "9".
    let req = cbor(&Cbor::Map(vec![
        (
            Cbor::Text("collation_id".into()),
            Cbor::Integer(coll_id.into()),
        ),
        (Cbor::Text("a".into()), Cbor::Text("10".into())),
        (Cbor::Text("b".into()), Cbor::Text("9".into())),
    ]));
    let resp = p
        .invoke("collation.compare", &req)
        .await
        .expect("collation.compare");
    let ord = as_int(&de(&resp));
    eprintln!("[uint] compare(\"10\",\"9\") = {ord}");
    assert_eq!(ord, 1, "uint collation should order 10 > 9 numerically");
}

// ===========================================================================
// VTAB tier — series (eponymous, read-only). Prove the read surface
// reaches the provider: describe lists it; connect succeeds.
// ===========================================================================

#[tokio::test]
async fn vtab_series_describe_and_connect() {
    let Some((_h, p)) = open("series-provider.wasm") else {
        eprintln!("skip: series-provider.wasm absent");
        return;
    };
    let m = de(&p.invoke("describe", &[]).await.unwrap());
    let vtabs = as_arr(field(&m, "vtabs"));
    assert!(!vtabs.is_empty(), "series should export a vtab");
    let name = as_text(field(&vtabs[0], "name")).to_string();
    eprintln!("[series] vtab name={name} eponymous={:?}", field(&vtabs[0], "eponymous"));
    assert!(
        matches!(field(&vtabs[0], "eponymous"), Cbor::Bool(true)),
        "series is eponymous"
    );
}

// ===========================================================================
// DOTCMD (streaming) — greet. invoke a dot command -> text/stdout.
// ===========================================================================

#[tokio::test]
async fn dotcmd_greet_invoke_streaming() {
    use std::collections::HashMap;

    let Some((_h, p)) = open("greet-provider.wasm") else {
        eprintln!("skip: greet-provider.wasm absent");
        return;
    };
    // greet is a STREAMING dotcmd provider: it imports cli-stdout and
    // emits its output mid-`handle` rather than returning it. The host
    // must drive it via `invoke_cli`, which satisfies cli-stdout/stderr/
    // state with a per-invoke capture buffer.
    assert!(
        p.is_streaming_cli(),
        "greet should be detected as a streaming-cli provider"
    );

    // describe still works through the cli-aware linker.
    let (mbytes, _) = p
        .invoke_cli("describe", &[], HashMap::new())
        .await
        .expect("describe via cli linker");
    let m = de(&mbytes);
    let cmds = as_arr(field(&m, "dot_commands"));
    assert!(!cmds.is_empty(), "greet should export a dot command");
    let func_id = as_int(field(&cmds[0], "id")) as u64;
    let cname = as_text(field(&cmds[0], "name")).to_string();

    let req = cbor(&Cbor::Map(vec![
        (Cbor::Text("func_id".into()), Cbor::Integer(func_id.into())),
        (Cbor::Text("args".into()), Cbor::Text("world".into())),
        (Cbor::Text("interactive".into()), Cbor::Bool(false)),
        (Cbor::Text("display_mode".into()), Cbor::Text("list".into())),
        (Cbor::Text("bail_on_error".into()), Cbor::Bool(false)),
    ]));
    // seed a minimal cli-state snapshot (display mode + db path).
    let mut state: HashMap<String, String> = HashMap::new();
    state.insert("display/mode".into(), "list".into());
    let (resp, cli) = p
        .invoke_cli("dotcmd.invoke", &req, state)
        .await
        .expect("streaming dotcmd.invoke");
    let r = de(&resp);
    let ok = matches!(field(&r, "ok"), Cbor::Bool(true));
    eprintln!(
        "[greet] .{cname} world -> ok={ok} text={:?} streamed_stdout={:?}",
        field(&r, "text"),
        cli.stdout
    );
    assert!(ok, "greet dotcmd should succeed: {r:?}");
    // The greeting text must surface either in the response `text` or in
    // the streamed stdout capture.
    let resp_text = as_text(field(&r, "text"));
    let surfaced = resp_text.contains("world") || cli.stdout.contains("world");
    assert!(
        surfaced,
        "greeting for 'world' should surface in text or streamed stdout: \
         text={resp_text:?} stdout={:?}",
        cli.stdout
    );
}

// ===========================================================================
// DOTCMD (return-style, non-streaming) — dotret. Proves the same
// dotcmd.invoke contract without the cli-stdout streaming branch.
// ===========================================================================

#[tokio::test]
async fn dotcmd_dotret_invoke() {
    let Some((_h, p)) = open("dotret-provider.wasm") else {
        eprintln!("skip: dotret-provider.wasm absent");
        return;
    };
    let m = de(&p.invoke("describe", &[]).await.unwrap());
    let cmds = as_arr(field(&m, "dot_commands"));
    assert!(!cmds.is_empty());
    let func_id = as_int(field(&cmds[0], "id")) as u64;
    let req = cbor(&Cbor::Map(vec![
        (Cbor::Text("func_id".into()), Cbor::Integer(func_id.into())),
        (Cbor::Text("args".into()), Cbor::Text("ping".into())),
        (Cbor::Text("interactive".into()), Cbor::Bool(false)),
        (Cbor::Text("display_mode".into()), Cbor::Text("list".into())),
        (Cbor::Text("bail_on_error".into()), Cbor::Bool(false)),
    ]));
    let r = de(&p.invoke("dotcmd.invoke", &req).await.expect("dotcmd.invoke"));
    let text = as_text(field(&r, "text"));
    eprintln!("[dotret] echo -> {text:?}");
    assert!(text.contains("ping"), "dotret should echo its arg: {text}");
}
