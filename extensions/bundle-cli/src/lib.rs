//! `.bundle`  named extension sets + cached baked binaries on
//! the host's cas-cache. v1 covers the full metadata + build
//! round-trip: save the current connection's loaded extensions
//! as a named bundle, then build or auto-build a per-target
//! binary via `spi.spawn-build` (which under the hood drives
//! `cargo build -p sqlite-cli --features embed-X,embed-Y,...`
//! and  for wasm targets  `wasm-tools component new`).
//!
//! Subcommands:
//!   .bundle save NAME [--no-build]   record live-connection's loaded exts
//!   .bundle build NAME [--target X]  build baked binary for target
//!   .bundle list                     all bundles, last-used desc
//!   .bundle show NAME|HASH           members + binaries
//!   .bundle delete NAME              drop bundle row + cascade
//!   .bundle gc [--keep N | --older-than DURATION]
//!
//! Capability surface: `Spi` (none currently used  reserved for
//! future SQL projections), `Bundles` (every CRUD call), and
//! `SpawnBuild` declared in the manifest as the upper bound (the
//! operator-side grant gates whether `.bundle save` without
//! `--no-build` / `.bundle build` succeed).

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "dotcmd-aware",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::dot_command::{
        Guest as DotCommandGuest, InvokeContext, InvokeResult,
    };
    use bindings::exports::sqlite::extension::metadata::{
        DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::build;
    use bindings::sqlite::extension::bundles;
    use bindings::sqlite::extension::cli_stdout;
    use bindings::sqlite::extension::loader_bridge;
    use bindings::sqlite::extension::policy::Capability;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    // SQLite primary result code returned by spi.spawn-build when
    // SpawnBuild is declared but not granted at load time. Used
    // to translate the host-side perm error into the Gap C user-
    // facing message.
    const SQLITE_PERM: i32 = 3;

    const FID_BUNDLE: u64 = 1;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "bundle-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![DotCommandSpec {
                    id: FID_BUNDLE,
                    name: "bundle".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    summary: "Named extension sets + cached baked binaries".into(),
                    usage: "bundle SUB [args]".into(),
                    help: BUNDLE_HELP.into(),
                    examples: alloc::vec![],
                    requires_write: false,
                    no_args: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                // Required: Bundles  every CRUD dot-cmd routes
                // through the bundles SPI dispatcher.
                declared_capabilities: alloc::vec![Capability::Bundles],
                // Optional: SpawnBuild  only `.bundle build` needs
                // it; calling without the grant returns SQLITE_PERM
                // which the build path translates into the Gap C
                // user-facing message. Declared as optional so
                // bundle-cli still loads in the default cli (where
                // SpawnBuild is not granted) and lets .bundle save /
                // list / show / delete / gc work normally.
                optional_capabilities: alloc::vec![Capability::SpawnBuild],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("bundle-cli: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            if func_id != FID_BUNDLE {
                return Err(SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("bundle-cli: unknown func id {func_id}"),
                });
            }
            Ok(dispatch(ctx.args.trim()))
        }
    }

    const BUNDLE_HELP: &str = "\
.bundle SUB [args]

  save NAME [--no-build]   Record currently-loaded extensions as
                           bundle NAME. Without --no-build, also
                           builds a per-target binary for the
                           current host triple (requires
                           --grant spawn-build).
  list                     Show every bundle, last-used descending.
  show NAME|HASH           Members + binaries for one bundle.
                           NAME is an exact-match; HASH does
                           prefix lookup.
  delete NAME              Drop the bundle and its members /
                           binaries. Cas-cache artifacts (the
                           extension bytes) are NOT removed.
  build NAME [--target X]  Build a baked binary for the current
                           or specified target. Requires
                           --grant spawn-build. Cache-hit if
                           the (bundle, target) binary already
                           exists.
  gc [--keep N | --older-than 30d]
                           Prune via LRU or age policy.

Bundles are stored in the host's cas-cache. Identical extension
sets dedupe automatically  multiple names may alias the same
underlying set-hash row.";

    fn dispatch(arg: &str) -> InvokeResult {
        let mut toks = arg.split_whitespace();
        let sub = match toks.next() {
            Some(s) => s,
            None => return err("usage: .bundle SUB [args]  (try `.bundle list`)".into()),
        };
        let rest: Vec<&str> = toks.collect();
        match sub {
            "save" => sub_save(&rest),
            "list" => sub_list(),
            "show" => sub_show(&rest),
            "delete" => sub_delete(&rest),
            "build" => sub_build(&rest),
            "gc" => sub_gc(&rest),
            other => err(format!(
                ".bundle: unknown subcommand {other:?} (valid: save, list, show, delete, build, gc)"
            )),
        }
    }

    fn sub_save(args: &[&str]) -> InvokeResult {
        let mut name: Option<String> = None;
        let mut no_build = false;
        for a in args {
            match *a {
                "--no-build" => no_build = true,
                "--name" => {} // alias for next positional; swallow
                other if other.starts_with("--") => {
                    return err(format!(".bundle save: unknown flag {other:?}"));
                }
                other => {
                    if name.is_some() {
                        return err(".bundle save: only one NAME positional accepted".into());
                    }
                    name = Some(other.to_string());
                }
            }
        }
        let name = match name {
            Some(n) => n,
            None => return err(".bundle save: NAME required (usage: .bundle save NAME [--no-build])".into()),
        };
        // Filter out the auto-loaded cli-family extensions  these
        // are baked into every sqlink-cli build by embed_core_dotcmd
        // and don't have `embed-*` feature flags in cli/Cargo.toml.
        // Including them in the bundle would (a) make every bundle's
        // set-hash depend on the cli build's roster (and re-shuffle
        // on every cli rebuild), (b) cause duplicate-load errors at
        // --bundle-load time, and (c) make `.bundle build` fail
        // with "unknown feature embed-archive-cli" etc. Bundles
        // are *user-loaded* extensions only.
        let members: Vec<_> = loader_bridge::list_loaded_extensions()
            .into_iter()
            .filter(|m| !is_auto_loaded_cli_family(&m.name))
            .collect();
        if members.is_empty() {
            return err(
                ".bundle save: no user-loaded extensions to bundle. \
                 Run `.load <path/to/extension.component.wasm>` first \
                 (the auto-loaded cli-family extensions are excluded \
                 since they're baked into every sqlink-cli build)."
                    .into(),
            );
        }
        // Compute the canonical set-hash: blake3 of "{ext_name}\n{digest}\n"
        // for every member, sorted ascending by extension_name. The host
        // returns the list already name-sorted so we can hash directly.
        let mut hasher = blake3::Hasher::new();
        for m in &members {
            hasher.update(m.name.as_bytes());
            hasher.update(b"\n");
            hasher.update(m.digest.as_bytes());
            hasher.update(b"\n");
        }
        let set_hash = hasher.finalize().to_hex().to_string();

        let wit_members: Vec<bundles::BundleMember> = members
            .iter()
            .map(|m| bundles::BundleMember {
                extension_name: m.name.clone(),
                content_hash: m.digest.clone(),
            })
            .collect();

        let id = match bundles::bundle_save(Some(&name), &set_hash, &wit_members) {
            Ok(id) => id,
            Err(e) => return err(format!(".bundle save: {}", e.message)),
        };

        let mut out = format!(
            "bundle '{name}' saved (id={id}, set_hash={hash_prefix}, members={n})\n",
            hash_prefix = &set_hash[..16.min(set_hash.len())],
            n = members.len(),
        );
        for m in &members {
            out.push_str(&format!("  {}  {}\n", &m.digest[..16.min(m.digest.len())], m.name));
        }
        if !no_build {
            // Cache-hit: if a binary for the current host target
            // already exists (e.g. a prior `.bundle save` for an
            // identical set under a different name), skip cargo.
            let host_target = loader_bridge::host_target_triple();
            let detail = match bundles::bundle_show(id) {
                Ok(d) => d,
                Err(e) => {
                    cli_stdout::write(&out);
                    return err(format!(
                        ".bundle save: build follow-on failed loading bundle: {}",
                        e.message
                    ));
                }
            };
            if let Some(existing) = detail
                .binaries
                .iter()
                .find(|b| b.target_triple == host_target)
            {
                out.push_str(&format!(
                    "\nbinary already cached for {host_target}: {}\n",
                    existing.binary_path
                ));
            } else {
                match do_build(&name, id, &detail.members, &host_target) {
                    Ok(path) => {
                        out.push_str(&format!(
                            "\nbuilt binary for {host_target}: {path}\n"
                        ));
                    }
                    Err(e) => {
                        // Print the metadata-save success first so
                        // the operator can see the bundle was
                        // recorded; the build failure is a
                        // secondary error.
                        cli_stdout::write(&out);
                        return err(e);
                    }
                }
            }
        }
        cli_stdout::write(&out);
        ok()
    }

    fn sub_list() -> InvokeResult {
        let rows = match bundles::bundle_list() {
            Ok(r) => r,
            Err(e) => return err(format!(".bundle list: {}", e.message)),
        };
        if rows.is_empty() {
            cli_stdout::write("(no bundles)\n");
            return ok();
        }
        let mut out = String::new();
        out.push_str("NAME                 SET-HASH         MEMBERS  BINARIES  LAST-USED\n");
        for s in &rows {
            let name = s.name.clone().unwrap_or_else(|| "(unnamed)".to_string());
            out.push_str(&format!(
                "{:<20} {:<16} {:>7}  {:>8}  {}\n",
                truncate(&name, 20),
                &s.set_hash[..16.min(s.set_hash.len())],
                s.member_count,
                s.binary_count,
                s.last_used_at,
            ));
        }
        cli_stdout::write(&out);
        ok()
    }

    fn sub_show(args: &[&str]) -> InvokeResult {
        let key = match args.first() {
            Some(k) => *k,
            None => return err(".bundle show: NAME or HASH-PREFIX required".into()),
        };
        // Try exact name first, then hash-prefix.
        let summary = match bundles::bundle_find_by_name(key) {
            Ok(Some(s)) => s,
            Ok(None) => match bundles::bundle_find_by_hash_prefix(key) {
                Ok(v) if v.len() == 1 => v.into_iter().next().unwrap(),
                Ok(v) if v.is_empty() => {
                    return err(format!(".bundle show: no bundle matches {key:?}"));
                }
                Ok(_) => {
                    return err(format!(
                        ".bundle show: {key:?} is an ambiguous hash prefix; use more chars"
                    ));
                }
                Err(e) => return err(format!(".bundle show: {}", e.message)),
            },
            Err(e) => return err(format!(".bundle show: {}", e.message)),
        };
        let detail = match bundles::bundle_show(summary.id) {
            Ok(d) => d,
            Err(e) => return err(format!(".bundle show: {}", e.message)),
        };
        let mut out = String::new();
        let name = detail.summary.name.clone().unwrap_or_else(|| "(unnamed)".to_string());
        out.push_str(&format!(
            "bundle {name} (id={id})\n  set_hash:   {hash}\n  created_at: {ca}\n  last_used:  {lu}\n",
            id = detail.summary.id,
            hash = detail.summary.set_hash,
            ca = detail.summary.created_at,
            lu = detail.summary.last_used_at,
        ));
        out.push_str(&format!("  members ({}):\n", detail.members.len()));
        for m in &detail.members {
            out.push_str(&format!("    {}  {}\n", &m.content_hash[..16.min(m.content_hash.len())], m.extension_name));
        }
        out.push_str(&format!("  binaries ({}):\n", detail.binaries.len()));
        if detail.binaries.is_empty() {
            out.push_str("    (none baked  use `sqlink --bundle` for dynamic-load)\n");
        }
        for b in &detail.binaries {
            out.push_str(&format!("    {} -> {}\n", b.target_triple, b.binary_path));
        }
        bundles::bundle_touch(detail.summary.id);
        cli_stdout::write(&out);
        ok()
    }

    fn sub_delete(args: &[&str]) -> InvokeResult {
        let name = match args.first() {
            Some(n) => *n,
            None => return err(".bundle delete: NAME required".into()),
        };
        let summary = match bundles::bundle_find_by_name(name) {
            Ok(Some(s)) => s,
            Ok(None) => return err(format!(".bundle delete: bundle {name:?} not found")),
            Err(e) => return err(format!(".bundle delete: {}", e.message)),
        };
        match bundles::bundle_delete(summary.id) {
            Ok(()) => {
                cli_stdout::write(&format!("bundle '{name}' deleted (id={})\n", summary.id));
                ok()
            }
            Err(e) => err(format!(".bundle delete: {}", e.message)),
        }
    }

    fn sub_build(args: &[&str]) -> InvokeResult {
        let mut name: Option<String> = None;
        let mut target_override: Option<String> = None;
        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "--target" => {
                    i += 1;
                    if i >= args.len() {
                        return err(".bundle build: --target expects a triple".into());
                    }
                    target_override = Some(args[i].to_string());
                }
                other if other.starts_with("--") => {
                    return err(format!(".bundle build: unknown flag {other:?}"));
                }
                other => {
                    if name.is_some() {
                        return err(".bundle build: only one NAME positional accepted".into());
                    }
                    name = Some(other.to_string());
                }
            }
            i += 1;
        }
        let name = match name {
            Some(n) => n,
            None => return err(".bundle build: NAME required (usage: .bundle build NAME [--target TRIPLE])".into()),
        };
        let summary = match bundles::bundle_find_by_name(&name) {
            Ok(Some(s)) => s,
            Ok(None) => return err(format!(".bundle build: bundle {name:?} not found")),
            Err(e) => return err(format!(".bundle build: {}", e.message)),
        };
        let detail = match bundles::bundle_show(summary.id) {
            Ok(d) => d,
            Err(e) => return err(format!(".bundle build: {}", e.message)),
        };
        let target = target_override
            .unwrap_or_else(loader_bridge::host_target_triple);
        // Cache-hit: if a binary for this (bundle, target) already
        // exists, return it without re-invoking cargo. Same-set
        // bundles share `set_hash` and therefore share their
        // bundle_binaries rows.
        if let Some(existing) = detail
            .binaries
            .iter()
            .find(|b| b.target_triple == target)
        {
            bundles::bundle_touch(summary.id);
            cli_stdout::write(&format!(
                "bundle '{name}' already built for {target}: {}\n",
                existing.binary_path
            ));
            return ok();
        }
        match do_build(&name, summary.id, &detail.members, &target) {
            Ok(path) => {
                cli_stdout::write(&format!(
                    "bundle '{name}' built for {target}: {path}\n"
                ));
                ok()
            }
            Err(e) => err(e),
        }
    }

    /// Shared build logic used by `.bundle build` and the with-build
    /// path of `.bundle save`. Returns the absolute host path of the
    /// produced binary (or component, for wasm targets) on success;
    /// returns a user-facing error string on failure  including the
    /// Gap C translation when spawn-build's host returns SQLITE_PERM.
    fn do_build(
        name: &str,
        bundle_id: u64,
        members: &[bundles::BundleMember],
        target: &str,
    ) -> Result<String, String> {
        let crate_root = resolve_crate_root()?;
        // Feature naming convention mirrors `sqlink compose --embed`:
        // each extension name X with underscores normalized to
        // hyphens becomes feature `embed-X`. See
        // host/src/main.rs run_compose_subcommand for the source
        // of truth.
        let features: Vec<String> = members
            .iter()
            .map(|m| format!("embed-{}", m.extension_name.replace('_', "-")))
            .collect();
        let out = match build::spawn_build(
            &crate_root,
            Some(target),
            &[],
            Some("sqlite-cli"),
            &features,
        ) {
            Ok(o) => o,
            Err(e) => {
                if e.code == SQLITE_PERM {
                    return Err(format!(
                        ".bundle build: spawn-build capability not granted. \
                         Re-run with `sqlink --grant spawn-build`, or use \
                         `.bundle save {name} --no-build` to record metadata only."
                    ));
                }
                return Err(format!(".bundle build: {}", e.message));
            }
        };
        if let Err(e) = bundles::bundle_record_binary(
            bundle_id,
            target,
            &out.binary_path,
        ) {
            return Err(format!(
                ".bundle build: produced {} but bundle-record-binary failed: {}",
                out.binary_path, e.message
            ));
        }
        Ok(out.binary_path)
    }

    /// Mirror of cli/src/lib.rs's embed_core_dotcmd auto-load list.
    /// These extensions are baked into every sqlink-cli build via
    /// include_bytes! and don't have `embed-*` feature flags in
    /// cli/Cargo.toml, so they can't appear in a bundle's
    /// `--features` list when `.bundle build` invokes cargo. They're
    /// also already-present in any baked binary, so omitting them
    /// is semantically correct: a bundle of {uuid, json1} run by
    /// the cli still has archive-cli, session-cli, etc. available.
    /// Keep this list in sync with embed_core_dotcmd().
    fn is_auto_loaded_cli_family(name: &str) -> bool {
        matches!(
            name,
            "bundle-cli"
                | "core-dotcmd"
                | "sqlink-meta-cli"
                | "sha3sum-cli"
                | "serialize-cli"
                | "archive-cli"
                | "session-cli"
                | "sqlite-utils-schema"
                | "sqlite-utils-data"
                | "sqlite-utils-fts"
                | "sqlite-utils-maint"
        )
    }

    /// Resolve the sqlink workspace root for the cargo invocation.
    ///
    /// Resolution order (plan's open-question decision #2 +
    /// v1.1 `loader-bridge.env-var` substrate):
    ///   1. `$SQLINK_DEV_ROOT` if set and non-empty (via the
    ///      loader-bridge env-var bridge call). Honored as-is
    ///      if the user explicitly set it, a typo should fail
    ///      cargo-loudly, not silently fall back.
    ///   2. Compile-time path derived from `CARGO_MANIFEST_DIR`:
    ///      strip `/extensions/bundle-cli` to recover the workspace
    ///      root. Always correct in dev (sqlink built from its
    ///      own workspace).
    ///
    /// On a clean install where neither resolves, the error names
    /// both the env var and the expected compile-time path so the
    /// operator can pick whichever they prefer.
    fn resolve_crate_root() -> Result<String, String> {
        if let Some(dev_root) = loader_bridge::env_var("SQLINK_DEV_ROOT") {
            return Ok(dev_root);
        }
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let mut buf = manifest_dir.to_string();
        for needle in ["/extensions/bundle-cli", "\\extensions\\bundle-cli"] {
            if let Some(idx) = buf.rfind(needle) {
                buf.truncate(idx);
                return Ok(buf);
            }
        }
        Err(format!(
            ".bundle build: SQLINK_DEV_ROOT unset and compile-time \
             workspace path missing (expected: {manifest_dir:?} ending \
             in extensions/bundle-cli/). Set SQLINK_DEV_ROOT to your \
             sqlink source checkout, or rebuild bundle-cli from the \
             sqlink workspace."
        ))
    }

    fn sub_gc(args: &[&str]) -> InvokeResult {
        let mut keep_last: Option<u32> = None;
        let mut older_than_secs: Option<u64> = None;
        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "--keep" => {
                    i += 1;
                    if i >= args.len() {
                        return err(".bundle gc: --keep expects a count".into());
                    }
                    keep_last = match args[i].parse() {
                        Ok(n) => Some(n),
                        Err(_) => return err(format!(".bundle gc: --keep: not an integer: {:?}", args[i])),
                    };
                }
                "--older-than" => {
                    i += 1;
                    if i >= args.len() {
                        return err(".bundle gc: --older-than expects a duration (e.g. 30d, 12h, 86400s)".into());
                    }
                    older_than_secs = match parse_duration(args[i]) {
                        Ok(n) => Some(n),
                        Err(e) => return err(format!(".bundle gc: --older-than: {e}")),
                    };
                }
                other => return err(format!(".bundle gc: unknown flag {other:?}")),
            }
            i += 1;
        }
        if keep_last.is_none() && older_than_secs.is_none() {
            return err(".bundle gc: pass --keep N or --older-than DURATION (e.g. 30d)".into());
        }
        let policy = bundles::GcPolicy { keep_last, older_than_secs };
        let dropped = match bundles::bundle_gc(policy) {
            Ok(d) => d,
            Err(e) => return err(format!(".bundle gc: {}", e.message)),
        };
        cli_stdout::write(&format!("dropped {} bundle(s): {:?}\n", dropped.len(), dropped));
        ok()
    }

    fn parse_duration(s: &str) -> Result<u64, String> {
        let (num, mul): (&str, u64) = if let Some(n) = s.strip_suffix('s') {
            (n, 1)
        } else if let Some(n) = s.strip_suffix('m') {
            (n, 60)
        } else if let Some(n) = s.strip_suffix('h') {
            (n, 3600)
        } else if let Some(n) = s.strip_suffix('d') {
            (n, 86400)
        } else {
            return Err(format!("expected a number with suffix s|m|h|d (got {s:?})"));
        };
        let n: u64 = num
            .parse()
            .map_err(|_| format!("not an integer: {num:?}"))?;
        Ok(n * mul)
    }

    fn truncate(s: &str, n: usize) -> String {
        if s.len() <= n {
            s.to_string()
        } else {
            let mut out = s.chars().take(n.saturating_sub(1)).collect::<String>();
            out.push('+');
            out
        }
    }

    fn err(msg: String) -> InvokeResult {
        InvokeResult {
            text: format!("{msg}\n"),
            state_deltas: alloc::vec![],
            ok: false,
            exit_code: 1,
        }
    }

    fn ok() -> InvokeResult {
        InvokeResult {
            text: String::new(),
            state_deltas: alloc::vec![],
            ok: true,
            exit_code: 0,
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
