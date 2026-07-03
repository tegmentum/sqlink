//! #220: the parser intercept (`__sqlink_parse` / `dispatch_parse`) must fire
//! for a PROVIDER-BACKED extension, not only a bespoke-loaded one. Provider-
//! backed exts live in `provider_manifests`, not `self.components`, so
//! `dispatch_parse` was extended to snapshot them too. This test loads the
//! ggsql extension as a compose:dynlink PROVIDER (`ggsql-provider.wasm`, the
//! ext wac-plugged into the scalar shape) and asserts the VISUALIZE rewrite
//! fires through the provider path — the provider-side equivalent of
//! `parser.rs::ggsql_visualize_parses_and_executes_in_sqlink`.
//!
//! Fixture: set SQLINK_PROV_STAGE to a dir holding `ggsql-provider.wasm`
//! (built out-of-band). Skips gracefully when absent, per repo convention.

use std::path::PathBuf;

use sqlink_host::{Host, Policy};

const VISUALIZE: &str =
    "VISUALIZE SELECT 'apple' AS label, 3 AS n UNION ALL SELECT 'pear' AS label, 1 AS n";

fn ggsql_provider_path() -> Option<PathBuf> {
    let stage = std::env::var_os("SQLINK_PROV_STAGE").map(PathBuf::from)?;
    let p = stage.join("ggsql-provider.wasm");
    p.exists().then_some(p)
}

#[tokio::test]
async fn ggsql_visualize_parses_through_the_provider_path() {
    let Some(path) = ggsql_provider_path() else {
        eprintln!("skipping: ggsql-provider.wasm not staged (set SQLINK_PROV_STAGE)");
        return;
    };

    let host = Host::new().expect("engine");
    // load_extension detects the endpoint export and routes onto the resident
    // provider path (NOT the bespoke loader) — so the ext is registered in
    // provider_manifests, not self.components.
    let name = host
        .load_extension(path, Policy::deny_all())
        .await
        .expect("load ggsql provider");
    assert_eq!(name, "ggsql");

    // The parser intercept must still fire for the provider-backed ext.
    let rewrite = host
        .dispatch_parse(VISUALIZE)
        .await
        .expect("dispatch_parse ok")
        .expect("provider-backed ggsql should claim VISUALIZE (#220 parser gap)");
    assert!(rewrite.contains("__viz"), "rewrite wraps the inner select: {rewrite}");
}
