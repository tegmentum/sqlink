//! `.session`  PLAN-cli-stages-5-6.md Stage 6: migrated out
//! of cli/src/dot.rs as a dot-command extension. Subcommand
//! dispatch lives here; the underlying sqlite3session_* state
//! is on the host (Host::session_handles), accessed via the
//! new spi.session-* WIT imports.

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
    use bindings::sqlite::extension::session;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_SESSION: u64 = 1;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "session-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![DotCommandSpec {
                    id: FID_SESSION,
                    name: "session".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    summary: "Capture & emit sqlite3_session changesets".into(),
                    usage: "session NAME {create|attach|enable|indirect|isempty|changeset|patchset|delete}\n\
                            .session list".into(),
                    help: "Manage named sqlite3_session handles on the host's shared connection.\n\
                           Subcommands:\n  \
                           NAME create [DB]       open a session on DB (default main)\n  \
                           NAME attach [TABLE]    attach a table (default = all tables)\n  \
                           NAME enable on|off     toggle change tracking\n  \
                           NAME indirect on|off   mark subsequent changes indirect\n  \
                           NAME isempty           print 1 if nothing captured, 0 otherwise\n  \
                           NAME changeset FILE    write changeset bytes to FILE\n  \
                           NAME patchset FILE     write patchset bytes to FILE\n  \
                           NAME delete            release the handle\n  \
                           list                   print every live session name".into(),
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
                preferred_prefix: Some("session".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.cli.session".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("session-cli: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            if func_id != FID_SESSION {
                return Err(SqliteError {
                    code: 1, extended_code: 1,
                    message: format!("session-cli: unknown func id {func_id}"),
                });
            }
            Ok(cmd_session(ctx.args.trim()))
        }
    }

    fn cmd_session(arg: &str) -> InvokeResult {
        if arg.is_empty() {
            return err(".session NAME {create|attach|enable|indirect|isempty|changeset|patchset|delete}\n\
                       .session list".into());
        }
        let mut parts = arg.splitn(2, char::is_whitespace);
        let first = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();
        if first == "list" {
            return cmd_list();
        }
        let name = first;
        let mut sub_parts = rest.splitn(2, char::is_whitespace);
        let sub = sub_parts.next().unwrap_or("").trim();
        let subarg = sub_parts.next().unwrap_or("").trim();
        match sub {
            "create" => cmd_create(name, subarg),
            "attach" => cmd_attach(name, subarg),
            "enable" => cmd_enable_or_indirect(name, subarg, /*indirect=*/ false),
            "indirect" => cmd_enable_or_indirect(name, subarg, /*indirect=*/ true),
            "isempty" => cmd_isempty(name),
            "changeset" => cmd_changeset(name, subarg, /*patchset=*/ false),
            "patchset" => cmd_changeset(name, subarg, /*patchset=*/ true),
            "delete" => cmd_delete(name),
            other => err(format!(
                ".session {name}: unknown subcommand {other:?}\n\
                 Valid: create attach enable indirect isempty changeset patchset delete"
            )),
        }
    }

    fn cmd_list() -> InvokeResult {
        let names = session::session_list();
        let body = if names.is_empty() {
            "(no active sessions)\n".to_string()
        } else {
            let mut s = String::new();
            for n in names { s.push_str(&n); s.push('\n'); }
            s
        };
        text(body)
    }

    fn cmd_create(name: &str, db_arg: &str) -> InvokeResult {
        let db = if db_arg.is_empty() { "main" } else { db_arg };
        match session::session_create(name, db) {
            Ok(()) => text(String::new()),
            Err(e) => err(format!(".session {name} create: {}", e.message)),
        }
    }

    fn cmd_attach(name: &str, tbl: &str) -> InvokeResult {
        let table: Option<String> = if tbl.is_empty() || tbl == "*" {
            None
        } else {
            Some(tbl.to_string())
        };
        match session::session_attach(name, table.as_deref()) {
            Ok(()) => text(String::new()),
            Err(e) => err(format!(".session {name} attach: {}", e.message)),
        }
    }

    fn cmd_enable_or_indirect(name: &str, val: &str, indirect: bool) -> InvokeResult {
        let on = match parse_on_off(val) {
            Some(b) => b,
            None => return err(format!(
                ".session {name} {}: expected on|off, got {val:?}",
                if indirect { "indirect" } else { "enable" }
            )),
        };
        let res = if indirect {
            session::session_indirect(name, on)
        } else {
            session::session_enable(name, on)
        };
        match res {
            Ok(()) => text(String::new()),
            Err(e) => err(format!(
                ".session {name} {}: {}",
                if indirect { "indirect" } else { "enable" },
                e.message
            )),
        }
    }

    fn cmd_isempty(name: &str) -> InvokeResult {
        match session::session_isempty(name) {
            Ok(b) => text(if b { "1\n".into() } else { "0\n".into() }),
            Err(e) => err(format!(".session {name} isempty: {}", e.message)),
        }
    }

    fn cmd_changeset(name: &str, file: &str, patchset: bool) -> InvokeResult {
        if file.is_empty() {
            return err(format!(
                ".session {name} {} FILE: missing file path",
                if patchset { "patchset" } else { "changeset" }
            ));
        }
        let bytes = if patchset {
            session::session_patchset(name)
        } else {
            session::session_changeset(name)
        };
        let blob = match bytes {
            Ok(b) => b,
            Err(e) => return err(format!(
                ".session {name} {}: {}",
                if patchset { "patchset" } else { "changeset" },
                e.message
            )),
        };
        match std::fs::write(file, &blob) {
            Ok(()) => text(format!("wrote {} bytes to {file}\n", blob.len())),
            Err(e) => err(format!(
                ".session {name} {}: write {file:?}: {e}",
                if patchset { "patchset" } else { "changeset" }
            )),
        }
    }

    fn cmd_delete(name: &str) -> InvokeResult {
        match session::session_delete(name) {
            Ok(()) => text(String::new()),
            Err(e) => err(format!(".session {name} delete: {}", e.message)),
        }
    }

    fn parse_on_off(s: &str) -> Option<bool> {
        match s.trim().to_lowercase().as_str() {
            "on" | "true" | "1" | "yes" => Some(true),
            "off" | "false" | "0" | "no" => Some(false),
            _ => None,
        }
    }

    fn text(body: String) -> InvokeResult {
        InvokeResult { text: body, state_deltas: alloc::vec![], ok: true, exit_code: 0 }
    }

    fn err(message: String) -> InvokeResult {
        InvokeResult {
            text: format!("Error: {message}\n"),
            state_deltas: alloc::vec![],
            ok: false,
            exit_code: 1,
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
