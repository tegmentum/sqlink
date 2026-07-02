//! #220: function-name collision handling on the PROVIDER path (the
//! provider-only analog of `load_collision.rs`, which drives the bespoke
//! `install_loaded_extension` path).
//!
//! Loads `math-provider.wasm` (the `math` extension wac-plugged into the
//! compose:dynlink scalar shape) provider-only. `math` registers `abs/1`
//! and `round/1`, both of which collide with SQLite builtins. The cli
//! registers a provider-backed extension's scalars from the manifest
//! returned by the loader (`provider_backed_bindings_manifest` ->
//! `manifest_for_provider`), so the collision must be resolved THERE:
//! a colliding scalar is exposed as `<ext>_<name>` (builtin preserved),
//! a non-colliding one keeps its bare name. Dispatch is keyed by func_id,
//! not name, so only the registered SQL name changes.
//!
//! Skips silently if the fixture isn't built.

use std::path::PathBuf;

use sqlink_host::{Host, Policy};

fn math_provider_path() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for c in [
        "tests/fixtures/providers/math-provider.wasm",
        "../target/wasm32-wasip2/release/math-provider.wasm",
    ] {
        let p = d.join(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_scalar_collision_is_prefixed() {
    let Some(path) = math_provider_path() else {
        eprintln!("skipping: math-provider.wasm not built");
        return;
    };

    // File-backed db: with_shared_spi_conn_open rejects :memory:.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("collision.db");

    let host = Host::new().expect("engine");
    host.set_db_path(db_path.to_str().unwrap());

    let name = host
        .load_extension(path, Policy::deny_all())
        .await
        .expect("load math-provider");
    assert_eq!(name, "math");

    // Open the shared spi conn so collision resolution can see the builtins
    // (`resolve_collision_free_name` checks `function_exists`).
    host.with_shared_spi_conn_open(|_| {})
        .expect("open shared spi conn");

    let m = host
        .provider_backed_bindings_manifest("math")
        .expect("math is provider-backed");
    let names: Vec<&str> = m
        .scalar_functions
        .iter()
        .map(|s| s.name.as_str())
        .collect();

    // Colliding scalars are exposed under `<ext>_<name>`; the bare builtin
    // names are NOT registered (so `abs`/`round` keep their builtins).
    assert!(
        names.contains(&"math_abs"),
        "abs collides with the builtin -> must register as math_abs; got {names:?}"
    );
    assert!(
        names.contains(&"math_round"),
        "round collides -> must register as math_round; got {names:?}"
    );
    assert!(
        !names.contains(&"abs"),
        "bare `abs` must not be registered (it would clobber the builtin); got {names:?}"
    );
    assert!(
        !names.contains(&"round"),
        "bare `round` must not be registered; got {names:?}"
    );
}
