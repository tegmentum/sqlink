//! Task #228 composed-cli `.load` matrix. Drives the REAL composed cli
//! binary (`cli_with_sqlite.component.wasm`) through `run_cli_capture`
//! and `.load`s each provider fixture across every tier — scalar /
//! collation / aggregate / vtab(read) / vtab(mutating) / dot-command.
//!
//! WHAT REMAINDER 1 UNBLOCKED (asserted here): the composed cli now
//! INSTANTIATES (its `sqlink:wasm/dispatch` import matches the host's
//! 35-func linker) and each `.load` runs the provider `describe` +
//! registers the tier at the host layer — the log shows
//! `[sqlink] <ext> loaded as compose:dynlink provider` +
//! `Loaded extension: <ext> ... (N registered: ...)`. Before the skew
//! fix the cli trapped on `SELECT 1` before any `.load` could run.
//!
//! KNOWN RESIDUAL (documented, NOT asserted callable): a `.load`'d
//! provider tier is registered at the HOST dispatch layer but its
//! SQLite trampoline is not yet installed on the composed cli's
//! IN-WASM sqlite-lib query connection, so `SELECT ext_fn(...)` /
//! `COLLATE ext` / `USING ext_vtab` still report "no such ...". This
//! is the composed-cli in-wasm-connection wiring seam — the "deep
//! self-callback tier" #227's close-out flagged as the last residual —
//! distinct from the host-layer dispatch, which the
//! `resident_provider_coherence` suite proves green (5/5) via the
//! direct `Host` API. This test is the legible record of that boundary.

use std::path::PathBuf;

fn composed_cli() -> Option<PathBuf> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../target/wasm32-wasip2/release/cli_with_sqlite.component.wasm");
    p.exists().then_some(p)
}

fn provider(name: &str) -> Option<PathBuf> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/providers");
    p.push(name);
    p.exists().then_some(p)
}

/// `.load` the given provider through the composed cli, then run `body`.
/// Returns the captured stdout, or None if the composed cli / fixture is
/// absent (the caller skips). Asserts the shared invariant remainder 1
/// bought: the composed cli instantiates and the provider loads.
async fn load_and_run(fixture: &str, body: &str) -> Option<String> {
    let cli = composed_cli()?;
    let p = provider(fixture)?;
    let script = format!(".load {}\n{body}.quit\n", p.display());
    let out = sqlink_host::run_cli_capture(":memory:", &cli, &script)
        .await
        .expect("run_cli_capture (composed cli must instantiate — remainder 1)");
    eprintln!("[{fixture}] ---\n{out}\n---");
    // Remainder-1 invariant: the composed cli ran and the provider
    // `.load` reached describe + tier registration.
    assert!(
        out.contains("loaded as compose:dynlink provider")
            && out.contains("Loaded extension:"),
        "{fixture}: composed cli should instantiate and .load the provider: {out}"
    );
    Some(out)
}

#[tokio::test(flavor = "multi_thread")]
async fn matrix_scalar_aba() {
    let Some(out) =
        load_and_run("aba-provider.wasm", "SELECT aba_validate('021000021');\n").await
    else {
        eprintln!("skip: aba-provider.wasm / composed cli absent");
        return;
    };
    assert!(out.contains("3 registered: 3 scalar"), "aba should register 3 scalars: {out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn matrix_collation_uint() {
    let Some(out) = load_and_run(
        "uint-provider.wasm",
        "CREATE TABLE t(x TEXT);\nINSERT INTO t VALUES ('9'),('10'),('100');\n\
         SELECT x FROM t ORDER BY x COLLATE uint;\n",
    )
    .await
    else {
        eprintln!("skip: uint-provider.wasm / composed cli absent");
        return;
    };
    assert!(out.contains("1 collation"), "uint should register a collation: {out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn matrix_aggregate_count_min() {
    let Some(out) = load_and_run(
        "count_min-provider.wasm",
        "CREATE TABLE t(x INTEGER);\nINSERT INTO t VALUES (1),(1),(2);\n\
         SELECT count_min(x) FROM t;\n",
    )
    .await
    else {
        eprintln!("skip: count_min-provider.wasm / composed cli absent");
        return;
    };
    assert!(out.contains("aggregate"), "count_min should register an aggregate: {out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn matrix_vtab_read_series() {
    let Some(out) = load_and_run(
        "series-provider.wasm",
        "SELECT count(*) FROM generate_series(1, 5);\n",
    )
    .await
    else {
        eprintln!("skip: series-provider.wasm / composed cli absent");
        return;
    };
    assert!(out.contains("vtab"), "series should register vtabs: {out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn matrix_vtab_mut_inmem() {
    let Some(out) = load_and_run(
        "inmem-provider.wasm",
        "CREATE VIRTUAL TABLE m USING inmem;\nINSERT INTO m VALUES ('hi');\n\
         SELECT count(*) FROM m;\nALTER TABLE m RENAME TO m2;\nPRAGMA integrity_check;\n",
    )
    .await
    else {
        eprintln!("skip: inmem-provider.wasm / composed cli absent");
        return;
    };
    assert!(out.contains("1 vtab"), "inmem should register a vtab: {out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn matrix_dotcmd_greet() {
    let Some(out) = load_and_run("greet-provider.wasm", ".greet alice\n").await else {
        eprintln!("skip: greet-provider.wasm / composed cli absent");
        return;
    };
    // greet registers no SQL tiers (0 functions) — the .load itself is
    // the proof the dotcmd-shape provider is accepted.
    assert!(!out.contains("panicked"), ".greet .load should not trap: {out}");
}
