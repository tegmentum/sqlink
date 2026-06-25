//! `.sqlink` meta-cli  Phase 5 follow-up: migrated out of
//! cli/src/dot.rs into a wasm dot-command extension.
//!
//! Implements the full subcommand surface against the
//! sqlink_dotcmd / sqlink_artifact / sqlink_cas_resolver tables
//! via `spi.execute`. Uses the new `loader-bridge` import for
//! the install path (which needs to call back into the host's
//! extension-loader to introspect bytes).

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
    use bindings::sqlite::extension::cli_stdout;
    use bindings::sqlite::extension::loader_bridge;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_SQLINK: u64 = 1;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "sqlink-meta-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![DotCommandSpec {
                    id: FID_SQLINK,
                    name: "sqlink".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    summary: "Manage the database-resident dot-command registry".into(),
                    usage: "sqlink SUB [args]".into(),
                    help: SQLINK_HELP.into(),
                    examples: alloc::vec![],
                    requires_write: false,
                    no_args: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("sqlink-meta-cli: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            if func_id != FID_SQLINK {
                return Err(SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("sqlink-meta-cli: unknown func id {func_id}"),
                });
            }
            let text = dispatch(ctx.args.trim());
            Ok(InvokeResult {
                text,
                state_deltas: alloc::vec![],
                ok: true,
                exit_code: 0,
            })
        }
    }

    const SQLINK_HELP: &str = "Subcommands:\n  \
        list                              every registered command\n  \
        show NAME                         row + manifest snippet\n  \
        install URI [--no-bundle]         load + register every dot-command in the extension\n  \
        uninstall NAME                    drop the registry row (artifact stays)\n  \
        bundle NAME                       (re-)cache bytes in sqlink_artifact\n  \
        unbundle NAME                     drop the artifact row (refcount-safe)\n  \
        bundle-all\n  \
        unbundle-all\n  \
        verify                            re-hash every artifact row; flag mismatches\n  \
        gc                                drop unreferenced artifact rows\n  \
        export NAME PATH                  write the wasm bytes to disk\n  \
        resolver list\n  \
        resolver add PRIORITY URI\n  \
        resolver remove URI\n  \
        resolver set-priority URI N\n";

    fn dispatch(arg: &str) -> String {
        let mut parts = arg.splitn(2, char::is_whitespace);
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();
        match sub {
            "" => SQLINK_HELP.to_string(),
            "list" => sqlink_list(),
            "show" => sqlink_show(rest),
            "install" => sqlink_install(rest),
            "uninstall" => sqlink_uninstall(rest),
            "bundle" => sqlink_bundle(rest),
            "unbundle" => sqlink_unbundle(rest),
            "bundle-all" => sqlink_bundle_all(),
            "unbundle-all" => sqlink_unbundle_all(),
            "verify" => sqlink_verify(),
            "gc" => sqlink_gc(),
            "export" => sqlink_export(rest),
            "resolver" => sqlink_resolver(rest),
            other => format!(".sqlink: unknown subcommand {other:?}\n{SQLINK_HELP}"),
        }
    }

    // ----- SQL helpers wrapping spi::execute -------------------------

    fn run_rows(sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, String> {
        spi::execute(sql, params)
            .map(|r| r.rows)
            .map_err(|e| e.message)
    }

    fn run_exec(sql: &str, params: &[SqlValue]) -> Result<i64, String> {
        spi::execute(sql, params)
            .map(|r| r.changes)
            .map_err(|e| e.message)
    }

    fn text(v: &SqlValue) -> String {
        match v {
            SqlValue::Text(s) => s.clone(),
            SqlValue::Integer(n) => n.to_string(),
            SqlValue::Real(r) => r.to_string(),
            SqlValue::Null => String::new(),
            SqlValue::Blob(b) => format!("<blob:{}>", b.len()),
        }
    }

    fn int(v: &SqlValue) -> i64 {
        if let SqlValue::Integer(n) = v { *n } else { 0 }
    }

    // ----- list ------------------------------------------------------

    fn sqlink_list() -> String {
        let sql = "SELECT name, summary, artifact_size, artifact_digest,
                       EXISTS(SELECT 1 FROM sqlink_artifact WHERE digest = sqlink_dotcmd.artifact_digest)
                   FROM sqlink_dotcmd ORDER BY name";
        let rows = match run_rows(sql, &[]) {
            Ok(r) => r,
            Err(e) => return format!(".sqlink list: {e}\n"),
        };
        if rows.is_empty() {
            return "(no commands installed)\n".into();
        }
        let mut out = String::new();
        out.push_str("NAME              BUNDLED   SIZE    SUMMARY\n");
        for row in rows {
            let name = text(row.first().unwrap_or(&SqlValue::Null));
            let summary = text(row.get(1).unwrap_or(&SqlValue::Null));
            let size = int(row.get(2).unwrap_or(&SqlValue::Null));
            let bundled = int(row.get(4).unwrap_or(&SqlValue::Null)) == 1;
            out.push_str(&format!(
                "{name:<17} {b:<8} {size:<7} {summary}\n",
                b = if bundled { "yes" } else { "no" },
            ));
        }
        out
    }

    // ----- show ------------------------------------------------------

    fn sqlink_show(name: &str) -> String {
        if name.is_empty() { return "Usage: .sqlink show NAME\n".into(); }
        let sql = "SELECT d.summary, d.help, d.source_uri, d.artifact_digest,
                          d.artifact_size, d.installed_at,
                          EXISTS(SELECT 1 FROM sqlink_artifact WHERE digest = d.artifact_digest)
                   FROM sqlink_dotcmd d WHERE d.name = ?1";
        let rows = match run_rows(sql, &[SqlValue::Text(name.into())]) {
            Ok(r) => r,
            Err(e) => return format!(".sqlink show: {e}\n"),
        };
        let Some(row) = rows.into_iter().next() else {
            return format!("(no row for {name:?})\n");
        };
        let summary = text(row.first().unwrap_or(&SqlValue::Null));
        let help = text(row.get(1).unwrap_or(&SqlValue::Null));
        let source_uri = text(row.get(2).unwrap_or(&SqlValue::Null));
        let digest = text(row.get(3).unwrap_or(&SqlValue::Null));
        let size = int(row.get(4).unwrap_or(&SqlValue::Null));
        let installed_at = text(row.get(5).unwrap_or(&SqlValue::Null));
        let bundled = int(row.get(6).unwrap_or(&SqlValue::Null)) == 1;
        let mut o = String::new();
        o.push_str(&format!("name:         {name}\n"));
        o.push_str(&format!("summary:      {summary}\n"));
        if !help.is_empty() { o.push_str(&format!("help:         {help}\n")); }
        o.push_str(&format!("digest:       {digest}\n"));
        o.push_str(&format!("size:         {size} bytes\n"));
        o.push_str(&format!("installed_at: {installed_at}\n"));
        o.push_str(&format!("source_uri:   {}\n",
            if source_uri.is_empty() { "(none)" } else { &source_uri }));
        o.push_str(&format!("bundled:      {}\n", if bundled { "yes" } else { "no" }));
        o
    }

    // ----- install ---------------------------------------------------

    fn sqlink_install(arg: &str) -> String {
        let mut bundle = true;
        let mut uri: Option<&str> = None;
        for tok in arg.split_whitespace() {
            match tok {
                "--no-bundle" => bundle = false,
                "--bundle" => bundle = true,
                other if !other.starts_with("--") => uri = Some(other),
                other => return format!(".sqlink install: unknown flag {other:?}\n"),
            }
        }
        let Some(uri) = uri else {
            return "Usage: .sqlink install URI [--no-bundle]\n".into();
        };
        let path: String = if let Some(p) = uri.strip_prefix("file://") {
            p.to_string()
        } else if uri.starts_with("http://") || uri.starts_with("https://") {
            return ".sqlink install: http(s) URIs deferred to a follow-up\n".into();
        } else if uri.starts_with("cas:") {
            return ".sqlink install: cas: URIs deferred to a follow-up\n".into();
        } else {
            uri.to_string()
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return format!(".sqlink install: read {path:?}: {e}\n"),
        };
        let digest_hex = blake3::hash(&bytes).to_hex().to_string();
        let digest = format!("blake3:{digest_hex}");
        let size = bytes.len() as i64;

        // Load through the host  this gives us a trimmed manifest
        // AND populates the session registry, so the just-installed
        // command resolves in this same session.
        let manifest = match loader_bridge::load_extension_from_bytes("", &bytes, &[]) {
            Ok(m) => m,
            Err(e) => return format!(".sqlink install: load failed: {} ({})\n", e.message, e.code),
        };
        if manifest.dot_commands.is_empty() {
            return format!(
                ".sqlink install: {} declares no dot commands (loaded but not registered)\n",
                manifest.name,
            );
        }

        // Bundle the artifact first; one row per dot-command after.
        if bundle {
            let sql = "INSERT OR IGNORE INTO sqlink_artifact (digest, size, bytes, source_uri)
                       VALUES (?1, ?2, ?3, ?4)";
            if let Err(e) = run_exec(sql, &[
                SqlValue::Text(digest.clone()),
                SqlValue::Integer(size),
                SqlValue::Blob(bytes.clone()),
                SqlValue::Text(uri.into()),
            ]) {
                return format!(".sqlink install: artifact insert: {e}\n");
            }
        }

        let mut installed: Vec<String> = Vec::new();
        let mut errs: Vec<String> = Vec::new();
        for dc in &manifest.dot_commands {
            let sql = "INSERT OR REPLACE INTO sqlink_dotcmd
                          (name, summary, help, func_id, requires_write,
                           artifact_digest, artifact_size, source_uri)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";
            let res = run_exec(sql, &[
                SqlValue::Text(dc.name.clone()),
                SqlValue::Text(dc.summary.clone()),
                SqlValue::Text(dc.help.clone()),
                SqlValue::Integer(dc.id as i64),
                SqlValue::Integer(if dc.requires_write { 1 } else { 0 }),
                SqlValue::Text(digest.clone()),
                SqlValue::Integer(size),
                SqlValue::Text(uri.into()),
            ]);
            match res {
                Ok(_) => installed.push(dc.name.clone()),
                Err(e) => errs.push(format!("{}: {}", dc.name, e)),
            }
        }
        let mut out = format!(
            "Installed {} from {} ({} bytes, digest {}):\n",
            manifest.name, uri, size, digest,
        );
        for n in &installed { out.push_str(&format!("  .{n}\n")); }
        if !errs.is_empty() {
            out.push_str("Errors:\n");
            for e in &errs { out.push_str(&format!("  {e}\n")); }
        }
        out
    }

    // ----- uninstall -------------------------------------------------

    fn sqlink_uninstall(name: &str) -> String {
        if name.is_empty() { return "Usage: .sqlink uninstall NAME\n".into(); }
        match run_exec(
            "DELETE FROM sqlink_dotcmd WHERE name = ?1",
            &[SqlValue::Text(name.into())],
        ) {
            Ok(0) => format!("(no row for {name:?})\n"),
            Ok(n) => format!("Uninstalled {} ({n} row{})\n", name, if n == 1 { "" } else { "s" }),
            Err(e) => format!(".sqlink uninstall {name}: {e}\n"),
        }
    }

    // ----- bundle / unbundle ----------------------------------------

    fn dotcmd_meta(name: &str) -> Option<(String, i64, String)> {
        let sql = "SELECT artifact_digest, artifact_size, source_uri FROM sqlink_dotcmd WHERE name = ?1";
        let rows = run_rows(sql, &[SqlValue::Text(name.into())]).ok()?;
        let row = rows.into_iter().next()?;
        Some((
            text(row.first()?),
            int(row.get(1)?),
            text(row.get(2)?),
        ))
    }

    fn artifact_bytes(digest: &str) -> Option<Vec<u8>> {
        let rows = run_rows(
            "SELECT bytes FROM sqlink_artifact WHERE digest = ?1",
            &[SqlValue::Text(digest.into())],
        ).ok()?;
        let row = rows.into_iter().next()?;
        if let SqlValue::Blob(b) = row.into_iter().next()? {
            Some(b)
        } else { None }
    }

    fn digest_refcount(digest: &str) -> i64 {
        let rows = run_rows(
            "SELECT COUNT(*) FROM sqlink_dotcmd WHERE artifact_digest = ?1",
            &[SqlValue::Text(digest.into())],
        ).unwrap_or_default();
        rows.into_iter().next()
            .and_then(|r| r.into_iter().next())
            .map(|v| int(&v))
            .unwrap_or(0)
    }

    fn sqlink_bundle(name: &str) -> String {
        if name.is_empty() { return "Usage: .sqlink bundle NAME\n".into(); }
        let Some((digest, _size, source_uri)) = dotcmd_meta(name) else {
            return format!("(no row for {name:?})\n");
        };
        if artifact_bytes(&digest).is_some() {
            return format!("{name}: already bundled\n");
        }
        // Re-read from source_uri (file:// only in v1).
        let path: Option<&str> = if let Some(p) = source_uri.strip_prefix("file://") {
            Some(p)
        } else if source_uri.starts_with('/') {
            Some(source_uri.as_str())
        } else { None };
        let bytes = match path {
            Some(p) => match std::fs::read(p) {
                Ok(b) => b,
                Err(e) => return format!("{name}: read {p:?}: {e}\n"),
            },
            None => return format!(
                "{name}: source_uri empty or non-file ({source_uri:?})  CAS walk not exposed to extensions yet\n"
            ),
        };
        let got = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        if got != digest {
            return format!("{name}: digest mismatch from {source_uri} ({got} != {digest})\n");
        }
        let sql = "INSERT OR REPLACE INTO sqlink_artifact (digest, size, bytes, source_uri)
                   VALUES (?1, ?2, ?3, ?4)";
        match run_exec(sql, &[
            SqlValue::Text(digest.clone()),
            SqlValue::Integer(bytes.len() as i64),
            SqlValue::Blob(bytes.clone()),
            SqlValue::Text(source_uri),
        ]) {
            Ok(_) => format!("{name}: bundled {} bytes ({digest})\n", bytes.len()),
            Err(e) => format!("{name}: store_artifact: {e}\n"),
        }
    }

    fn sqlink_unbundle(name: &str) -> String {
        if name.is_empty() { return "Usage: .sqlink unbundle NAME\n".into(); }
        let Some((digest, _, _)) = dotcmd_meta(name) else {
            return format!("(no row for {name:?})\n");
        };
        let refs = digest_refcount(&digest);
        if refs > 1 {
            return format!(
                "{name}: artifact shared by {refs} commands  refusing to unbundle. \
                 Use `.sqlink unbundle-all` or uninstall the others first.\n"
            );
        }
        match run_exec(
            "DELETE FROM sqlink_artifact WHERE digest = ?1",
            &[SqlValue::Text(digest.clone())],
        ) {
            Ok(0) => format!("{name}: artifact already gone\n"),
            Ok(_) => format!("{name}: unbundled ({digest})\n"),
            Err(e) => format!("{name}: drop_artifact: {e}\n"),
        }
    }

    fn sqlink_bundle_all() -> String {
        let rows = run_rows("SELECT name FROM sqlink_dotcmd ORDER BY name", &[]).unwrap_or_default();
        if rows.is_empty() { return "(no commands installed)\n".into(); }
        let mut out = String::new();
        for row in rows {
            if let Some(v) = row.first() {
                out.push_str(&sqlink_bundle(&text(v)));
            }
        }
        out
    }

    fn sqlink_unbundle_all() -> String {
        let rows = run_rows("SELECT digest FROM sqlink_artifact", &[]).unwrap_or_default();
        let mut dropped = 0i64;
        let mut errs: Vec<String> = Vec::new();
        for row in rows {
            if let Some(v) = row.first() {
                let d = text(v);
                match run_exec("DELETE FROM sqlink_artifact WHERE digest = ?1",
                    &[SqlValue::Text(d.clone())]) {
                    Ok(n) => dropped += n,
                    Err(e) => errs.push(format!("{d}: {e}")),
                }
            }
        }
        let mut out = format!("Dropped {dropped} artifact row{}\n",
            if dropped == 1 { "" } else { "s" });
        for e in &errs { out.push_str(&format!("  {e}\n")); }
        out
    }

    // ----- verify / gc / export -------------------------------------

    fn sqlink_verify() -> String {
        let rows = run_rows("SELECT digest, size, bytes FROM sqlink_artifact ORDER BY digest", &[])
            .unwrap_or_default();
        if rows.is_empty() { return "(no artifacts)\n".into(); }
        let mut ok = 0u64;
        let mut bad: Vec<String> = Vec::new();
        for row in rows {
            let digest = text(row.first().unwrap_or(&SqlValue::Null));
            let size = int(row.get(1).unwrap_or(&SqlValue::Null));
            let bytes: Vec<u8> = match row.get(2) {
                Some(SqlValue::Blob(b)) => b.clone(),
                _ => { bad.push(format!("{digest}: bytes column missing")); continue; }
            };
            if bytes.len() as i64 != size {
                bad.push(format!("{digest}: size column = {size} but blob is {} bytes", bytes.len()));
                continue;
            }
            let got = format!("blake3:{}", blake3::hash(&bytes).to_hex());
            if got == digest { ok += 1; }
            else { bad.push(format!("{digest}: hashes to {got}")); }
        }
        let mut out = format!("verify: {ok} ok, {} bad\n", bad.len());
        for b in &bad { out.push_str(&format!("  {b}\n")); }
        out
    }

    fn sqlink_gc() -> String {
        match run_exec(
            "DELETE FROM sqlink_artifact \
             WHERE digest NOT IN (SELECT artifact_digest FROM sqlink_dotcmd)",
            &[],
        ) {
            Ok(0) => "gc: nothing to drop\n".into(),
            Ok(n) => format!("gc: dropped {n} unreferenced artifact row{}\n",
                if n == 1 { "" } else { "s" }),
            Err(e) => format!("gc: {e}\n"),
        }
    }

    fn sqlink_export(arg: &str) -> String {
        let mut parts = arg.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").trim();
        let path = parts.next().unwrap_or("").trim();
        if name.is_empty() || path.is_empty() {
            return "Usage: .sqlink export NAME PATH\n".into();
        }
        let Some((digest, _, _)) = dotcmd_meta(name) else {
            return format!("(no row for {name:?})\n");
        };
        let Some(bytes) = artifact_bytes(&digest) else {
            return format!("{name}: not bundled (digest {digest})\n");
        };
        match std::fs::write(path, &bytes) {
            Ok(()) => format!("Wrote {} bytes to {path}\n", bytes.len()),
            Err(e) => format!(".sqlink export {name}: {e}\n"),
        }
    }

    // ----- resolver -------------------------------------------------

    fn sqlink_resolver(arg: &str) -> String {
        let mut parts = arg.splitn(2, char::is_whitespace);
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();
        match sub {
            "" | "list" => resolver_list(),
            "add" => resolver_add(rest),
            "remove" | "rm" => resolver_remove(rest),
            "set-priority" => resolver_set_priority(rest),
            other => format!(".sqlink resolver: unknown {other:?}\n"),
        }
    }

    fn resolver_list() -> String {
        let rows = run_rows(
            "SELECT priority, kind, uri FROM sqlink_cas_resolver ORDER BY priority",
            &[],
        ).unwrap_or_default();
        if rows.is_empty() { return "(no resolvers configured)\n".into(); }
        let mut o = String::new();
        o.push_str("PRIORITY  KIND    URI\n");
        for row in rows {
            let p = int(row.first().unwrap_or(&SqlValue::Null));
            let k = text(row.get(1).unwrap_or(&SqlValue::Null));
            let u = text(row.get(2).unwrap_or(&SqlValue::Null));
            o.push_str(&format!("{:<9} {:<7} {}\n", p, k, u));
        }
        o
    }

    fn resolver_add(arg: &str) -> String {
        let mut parts = arg.splitn(2, char::is_whitespace);
        let priority_s = parts.next().unwrap_or("").trim();
        let uri = parts.next().unwrap_or("").trim();
        if priority_s.is_empty() || uri.is_empty() {
            return "Usage: .sqlink resolver add PRIORITY URI\n".into();
        }
        let Ok(priority) = priority_s.parse::<i64>() else {
            return format!(".sqlink resolver add: bad priority {priority_s:?}\n");
        };
        let kind = if uri.starts_with("file://") || uri.starts_with('/') {
            "file"
        } else if uri.starts_with("http://") || uri.starts_with("https://") {
            "http"
        } else {
            return format!(".sqlink resolver add: cannot infer kind from {uri:?}\n");
        };
        match run_exec(
            "INSERT OR REPLACE INTO sqlink_cas_resolver (priority, kind, uri) VALUES (?1, ?2, ?3)",
            &[
                SqlValue::Integer(priority),
                SqlValue::Text(kind.into()),
                SqlValue::Text(uri.into()),
            ],
        ) {
            Ok(_) => format!("Added resolver: priority={priority} kind={kind} uri={uri}\n"),
            Err(e) => format!(".sqlink resolver add: {e}\n"),
        }
    }

    fn resolver_remove(uri: &str) -> String {
        if uri.is_empty() { return "Usage: .sqlink resolver remove URI\n".into(); }
        match run_exec(
            "DELETE FROM sqlink_cas_resolver WHERE uri = ?1",
            &[SqlValue::Text(uri.into())],
        ) {
            Ok(0) => format!("(no resolver row for {uri:?})\n"),
            Ok(_) => format!("Removed resolver {uri}\n"),
            Err(e) => format!(".sqlink resolver remove: {e}\n"),
        }
    }

    fn resolver_set_priority(arg: &str) -> String {
        let mut parts = arg.splitn(2, char::is_whitespace);
        let uri = parts.next().unwrap_or("").trim();
        let n_s = parts.next().unwrap_or("").trim();
        if uri.is_empty() || n_s.is_empty() {
            return "Usage: .sqlink resolver set-priority URI N\n".into();
        }
        let Ok(n) = n_s.parse::<i64>() else {
            return format!(".sqlink resolver set-priority: bad N {n_s:?}\n");
        };
        match run_exec(
            "UPDATE sqlink_cas_resolver SET priority = ?1 WHERE uri = ?2",
            &[SqlValue::Integer(n), SqlValue::Text(uri.into())],
        ) {
            Ok(0) => format!("(no resolver row for {uri:?})\n"),
            Ok(_) => format!("{uri}: priority -> {n}\n"),
            Err(e) => format!(".sqlink resolver set-priority: {e}\n"),
        }
    }

    // Silence dead-code warning  cli_stdout isn't used directly
    // (we return text via InvokeResult.text), but the import has
    // to be in scope for the bindings to resolve.
    #[allow(dead_code)]
    fn _keep_imports_live() { cli_stdout::write(""); }

    bindings::export!(Ext with_types_in bindings);
}
