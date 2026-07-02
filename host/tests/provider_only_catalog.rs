//! #220 Phase 3 gate #3 — provider-only catalog coverage measurement.
//!
//! Stages the published `<ext>-provider.wasm` compose:dynlink artifacts on
//! the resolver path (`SQLINK_EXT_DIR`) and attempts `Host::load_extension`
//! by BARE catalog name for every registry extension. Because the resolver
//! now prefers `<ext>-provider.wasm` (commit e3af34df) and `.load` routes an
//! endpoint-exporting artifact onto the resident provider path, a successful
//! load here == the extension is servable PROVIDER-ONLY (no bespoke loader).
//!
//! This is the coverage that gates retiring the bespoke `loaded::*` loader.
//! Set SQLINK_PROV_STAGE to the staging dir to run; skips gracefully if
//! unset / empty (fixtures are built out-of-band).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use sqlink_host::compose_provider::ProviderHandle;
use sqlink_host::{Capability, Host, Policy};
use sqlite_component_core::db;

fn registry_names(root: &PathBuf) -> Vec<String> {
    let p = root.join("registry/index.json");
    let Ok(bytes) = std::fs::read(&p) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    v.get("extensions")
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|e| {
                    let cs = e
                        .get("checksum")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default();
                    cs != "sha256:builtin" && cs != "sha256:unbuilt"
                })
                .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_only_catalog_coverage() {
    let Some(stage) = std::env::var_os("SQLINK_PROV_STAGE").map(PathBuf::from) else {
        eprintln!("skipping: SQLINK_PROV_STAGE unset (stage the <ext>-provider.wasm artifacts)");
        return;
    };
    // The resolver consults SQLINK_EXT_DIR first.
    std::env::set_var("SQLINK_EXT_DIR", &stage);

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../"); // repo root
    let names = registry_names(&root);
    assert!(!names.is_empty(), "registry/index.json yielded no names");

    let mut ok = Vec::new();
    let mut no_artifact = Vec::new();
    let mut fail: BTreeMap<String, Vec<String>> = BTreeMap::new(); // signature -> names

    // Temp db so spi-importing providers can register a sqlite-runtime.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    {
        let c = db::Connection::open(db_path.to_str().unwrap(), db::OpenFlags::DEFAULT).unwrap();
        c.execute_batch("CREATE TABLE t(x);").unwrap();
    }

    for (idx, name) in names.iter().enumerate() {
        if !stage.join(format!("{name}-provider.wasm")).is_file() {
            no_artifact.push(name.clone());
            continue;
        }
        let host = Host::new().expect("engine");
        // Give spi-importing providers a runtime to resolve.
        let conn = db::Connection::open(db_path.to_str().unwrap(), db::OpenFlags::DEFAULT).unwrap();
        host.register_compose_provider(
            "sqlite-runtime",
            ProviderHandle::new_sqlite_runtime(Arc::new(Mutex::new(Some(conn)))),
        );
        let policy = Policy::deny_all().with_grants([Capability::Text]);
        eprint!("[{}/{}] {name} ... ", idx + 1, names.len());
        // Per-load timeout so a streaming/cli provider that blocks on stdin
        // (or any hang) can't stall the whole catalog sweep.
        let loaded = tokio::time::timeout(
            std::time::Duration::from_secs(12),
            host.load_extension(PathBuf::from(name), policy),
        )
        .await;
        match loaded {
            Ok(Ok(_)) => {
                eprintln!("OK");
                ok.push(name.clone());
            }
            Ok(Err(e)) => {
                let msg = e.to_string();
                let sig = msg
                    .lines()
                    .next()
                    .unwrap_or(&msg)
                    .split_whitespace()
                    .take(6)
                    .collect::<Vec<_>>()
                    .join(" ");
                eprintln!("FAIL: {sig}");
                fail.entry(sig).or_default().push(name.clone());
            }
            Err(_) => {
                eprintln!("TIMEOUT");
                fail.entry("<timeout 12s>".into())
                    .or_default()
                    .push(name.clone());
            }
        }
    }

    let fail_total: usize = fail.values().map(|v| v.len()).sum();
    eprintln!("\n=== PROVIDER-ONLY CATALOG COVERAGE ===");
    eprintln!("catalog exts:      {}", names.len());
    eprintln!("PROVIDER_OK:       {}", ok.len());
    eprintln!("PROVIDER_FAIL:     {fail_total}");
    eprintln!("NO_ARTIFACT:       {} {:?}", no_artifact.len(), no_artifact);
    if !fail.is_empty() {
        eprintln!("\n-- failure signatures --");
        for (sig, exts) in &fail {
            eprintln!("[{}] {}: {:?}", exts.len(), sig, {
                let mut s = exts.clone();
                s.truncate(8);
                s
            });
        }
    }
    // Measurement test: always "passes" — the numbers are the deliverable.
    // (A future hard gate can assert PROVIDER_OK == catalog once green.)
}
