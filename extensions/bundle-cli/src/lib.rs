//! `.bundle`  named extension sets + cached baked binaries on
//! the host's cas-cache. v1 covers the metadata-only path
//! end-to-end; the with-build path is wired to the substrate
//! (spi.spawn-build) but returns a clear "v1.1: build
//! orchestration deferred" message pending the design call on
//! how to drive `sqlink compose` from a wasm extension.
//!
//! Subcommands:
//!   .bundle save NAME [--no-build]   record live-connection's loaded exts
//!   .bundle build NAME [--target X]  (v1.1) build baked binary
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
    use bindings::sqlite::extension::bundles;
    use bindings::sqlite::extension::cli_stdout;
    use bindings::sqlite::extension::loader_bridge;
    use bindings::sqlite::extension::policy::Capability;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

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
                // Bundles  every CRUD call. SpawnBuild  upper
                // bound for the build path (grant gated separately
                // at load time). Spi reserved for future SQL
                // projections off bundle metadata.
                declared_capabilities: alloc::vec![
                    Capability::Spi,
                    Capability::Bundles,
                    Capability::SpawnBuild,
                ],
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
                           builds a per-target binary (v1.1; v1
                           always behaves as --no-build with a
                           note).
  list                     Show every bundle, last-used descending.
  show NAME|HASH           Members + binaries for one bundle.
                           NAME is an exact-match; HASH does
                           prefix lookup.
  delete NAME              Drop the bundle and its members /
                           binaries. Cas-cache artifacts (the
                           extension bytes) are NOT removed.
  build NAME [--target X]  (v1.1) Build a baked binary for the
                           current or specified target.
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
        let members = loader_bridge::list_loaded_extensions();
        if members.is_empty() {
            return err(".bundle save: no extensions loaded; nothing to bundle".into());
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
            out.push_str(
                "\nnote: build path not yet wired in v1 (PLAN-bundles.md #446 \
                 punted on cargo-orchestration design); treating --no-build implicitly. \
                 Use `sqlink --bundle ");
            out.push_str(&name);
            out.push_str(" db.sqlite` to dynamic-load this set on next launch.\n");
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

    fn sub_build(_args: &[&str]) -> InvokeResult {
        err(".bundle build: build orchestration not yet wired in v1 \
             (PLAN-bundles.md #446 deferred the design call on how to \
              drive `sqlink compose` from inside a wasm extension). The \
              substrate is ready (spi.spawn-build from #445); v1.1 will \
              land the generated-crate template + cargo invocation. For \
              now, use `sqlink --bundle NAME db.sqlite` to dynamic-load \
              the bundle's extensions on next launch."
            .into())
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
