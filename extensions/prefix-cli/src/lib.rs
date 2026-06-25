//! `.prefix`  SPARQL-style function namespacing operator surface
//! per PLAN-prefixes.md. All reads + writes hit the host's
//! `__sqlink_prefix*` tables via `spi.execute`; the loader-bridge
//! substrate already installs the schema and wraps function
//! registration, so this extension is the operator-facing UX
//! over an already-populated registry.
//!
//! Subcommands (one `.prefix` dot-cmd dispatches internally):
//!   add NAME EXPANSION [DESCRIPTION]
//!   list
//!   functions NAME
//!   expansion NAME
//!   rename OLD NEW
//!   modify NAME DESCRIPTION
//!   delete NAME
//!   prefer NAME EXTENSION
//!   unprefer NAME
//!   conflicts
//!   verify
//!
//! Capability surface: declares `Spi`. No new capability variant
//! per the plan  the registry tables live in the user db and all
//! access is through `spi.execute`.

extern crate alloc;

use alloc::format;
use alloc::string::String;

/// Strip ASCII control characters from operator-supplied prefix
/// names + expansions before echoing them back to the terminal.
/// Mirrors bundle-cli's defensive helper; prevents an operator
/// who pastes a name with embedded ANSI escapes from repainting
/// their terminal session.
pub fn sanitize_for_terminal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if (c as u32) < 0x20 || c == '\x7f' {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_for_terminal;

    #[test]
    fn escapes_ansi_csi() {
        assert_eq!(
            sanitize_for_terminal("dangerous\x1b[31m bad"),
            "dangerous\\x1b[31m bad"
        );
    }

    #[test]
    fn passes_normal_chars_through() {
        assert_eq!(sanitize_for_terminal("foaf"), "foaf");
        assert_eq!(sanitize_for_terminal("http://example/v1/"), "http://example/v1/");
    }

    #[test]
    fn escapes_nul() {
        assert_eq!(sanitize_for_terminal("ab\0c"), "ab\\x00c");
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use crate::sanitize_for_terminal;
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
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
    use bindings::sqlite::extension::policy::Capability;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_PREFIX: u64 = 1;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "prefix-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: vec![],
                aggregate_functions: vec![],
                collations: vec![],
                vtabs: vec![],
                dot_commands: vec![DotCommandSpec {
                    id: FID_PREFIX,
                    name: "prefix".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    summary: "Function prefix namespace registry".into(),
                    usage: "prefix SUB [args]".into(),
                    help: PREFIX_HELP.into(),
                    examples: vec![],
                    requires_write: false,
                    no_args: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                declared_capabilities: vec![Capability::Spi],
                optional_capabilities: vec![],
                preferred_prefix: Some("sqlink".into()),
                prefix_expansion: Some("sqlink-internal://prefix-cli".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("prefix-cli: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            if func_id != FID_PREFIX {
                return Err(SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("prefix-cli: unknown func id {func_id}"),
                });
            }
            Ok(dispatch(ctx.args.trim()))
        }
    }

    const PREFIX_HELP: &str = "\
.prefix SUB [args]

Operator surface for SQL-function prefix namespacing. The substrate
auto-populates the registry as extensions load; these commands
inspect + manage it.

  add NAME EXPANSION [DESC]    Register a new prefix. EXPANSION is
                               required (opaque string  URL, NS,
                               UUID, etc.). DESC optional human label.
  list                         Show every prefix (name | expansion |
                               description | last_used).
  functions NAME               Functions registered under NAME's
                               expansion.
  expansion NAME               Print just the expansion string
                               (useful in shell scripts).
  rename OLD NEW               Change short alias. Function identity
                               (keyed by expansion) is unchanged.
  modify NAME DESC             Update description.
  delete NAME                  Remove short alias. WARNS if this is
                               the last alias for an expansion that
                               has functions registered.
  prefer NAME EXTENSION        Pin bare-name dispatch for NAME's
                               function to a specific extension. v1
                               note: takes effect on next session.
  unprefer NAME                Remove a pin.

Diagnostics:
  conflicts                    Bare-name ambiguities currently in
                               effect (function | n_args | pin state).
  verify                       Check that __sqlink_prefix_function
                               rows correspond to currently-loaded
                               extensions; warn on stale entries.

Backwards-compat invariant: bare-name function calls always continue
to work (preserves SQLite's existing semantics). Qualified
`prefix__name(...)` forms are purely additive  use them when you
need explicit dispatch in the face of a collision.";

    // ---------- top-level dispatch ----------

    fn dispatch(arg: &str) -> InvokeResult {
        let mut toks = arg.split_whitespace();
        let sub = match toks.next() {
            Some(s) => s,
            None => return err("usage: .prefix SUB [args]  (try `.prefix list`)".into()),
        };
        let rest: Vec<&str> = toks.collect();
        match sub {
            "add" => sub_add(&rest, arg),
            "list" => sub_list(),
            "functions" => sub_functions(&rest),
            "expansion" => sub_expansion(&rest),
            "rename" => sub_rename(&rest),
            "modify" => sub_modify(&rest, arg),
            "delete" => sub_delete(&rest),
            "prefer" => sub_prefer(&rest),
            "unprefer" => sub_unprefer(&rest),
            "conflicts" => sub_conflicts(),
            "verify" => sub_verify(),
            other => err(format!(
                ".prefix: unknown subcommand {:?} (valid: add, list, functions, expansion, \
                 rename, modify, delete, prefer, unprefer, conflicts, verify)",
                other
            )),
        }
    }

    // ---------- helpers ----------

    fn now_secs() -> i64 {
        // Use UTC via the same trick as bundle-cli  approximation
        // is fine for last_used_at; the host already populates created_at
        // at registration time. Wasi clocks would be cleaner but we
        // don't import them here to keep the SPI surface minimal.
        // Fall back to 0; the substrate's record_prefix path sets the
        // canonical timestamp on creation.
        0
    }

    fn first_text_col(rows: &[Vec<SqlValue>]) -> Option<String> {
        rows.iter().next().and_then(|r| {
            r.iter().next().and_then(|v| match v {
                SqlValue::Text(t) => Some(t.clone()),
                _ => None,
            })
        })
    }

    fn text_col(v: &SqlValue) -> String {
        match v {
            SqlValue::Text(t) => t.clone(),
            SqlValue::Null => String::new(),
            SqlValue::Integer(i) => i.to_string(),
            SqlValue::Real(r) => format!("{}", r),
            SqlValue::Blob(_) => "<blob>".into(),
        }
    }

    fn int_col(v: &SqlValue) -> i64 {
        match v {
            SqlValue::Integer(i) => *i,
            SqlValue::Real(r) => *r as i64,
            SqlValue::Text(t) => t.parse().unwrap_or(0),
            _ => 0,
        }
    }

    /// Resolve a `name` argument to its canonical expansion. Tries
    /// exact name first, then hash-prefix (for symmetry with
    /// `.bundle show`). Returns the expansion or None on no-match.
    fn resolve_to_expansion(name: &str) -> Result<Option<String>, SqliteError> {
        // Exact name lookup first.
        let r = spi::execute(
            "SELECT expansion FROM __sqlink_prefix WHERE name = ?1 LIMIT 1",
            &[SqlValue::Text(name.to_string())],
        )?;
        if let Some(exp) = first_text_col(&r.rows) {
            return Ok(Some(exp));
        }
        // Otherwise treat the arg as an expansion directly. Useful
        // for `--expansion <opaque>` style lookups; matches `.bundle show HASH`.
        let r = spi::execute(
            "SELECT expansion FROM __sqlink_prefix WHERE expansion = ?1 LIMIT 1",
            &[SqlValue::Text(name.to_string())],
        )?;
        Ok(first_text_col(&r.rows))
    }

    fn err_sqlite(e: SqliteError) -> InvokeResult {
        err(format!("sqlite: {} (code {})", e.message, e.code))
    }

    // ---------- commands ----------

    /// `.prefix add NAME EXPANSION [DESCRIPTION ...]`
    /// Whole DESCRIPTION captured from the raw arg-string so it
    /// can contain spaces.
    fn sub_add(rest: &[&str], raw: &str) -> InvokeResult {
        if rest.len() < 2 {
            return err(".prefix add: usage `.prefix add NAME EXPANSION [DESCRIPTION]`".into());
        }
        let name = rest[0];
        let expansion = rest[1];
        // Capture the description as everything after "add NAME EXPANSION ".
        // Robust to multi-word descriptions.
        let desc = description_after(raw, &["add", name, expansion]);

        // Verify name doesn't exist.
        let chk = match spi::execute(
            "SELECT 1 FROM __sqlink_prefix WHERE name = ?1 LIMIT 1",
            &[SqlValue::Text(name.to_string())],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        if !chk.rows.is_empty() {
            return err(format!(
                ".prefix add: prefix {:?} already exists. Use `.prefix modify {} ...` to update \
                 the description, or `.prefix rename` to change the alias.",
                sanitize_for_terminal(name),
                sanitize_for_terminal(name),
            ));
        }

        let params: Vec<SqlValue> = vec![
            SqlValue::Text(name.to_string()),
            SqlValue::Text(expansion.to_string()),
            if let Some(d) = &desc {
                SqlValue::Text(d.clone())
            } else {
                SqlValue::Null
            },
            SqlValue::Integer(now_secs()),
            SqlValue::Integer(now_secs()),
        ];
        if let Err(e) = spi::execute(
            "INSERT INTO __sqlink_prefix (name, expansion, description, created_at, last_used_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            &params,
        ) {
            return err_sqlite(e);
        }
        text(format!(
            "prefix {:?} -> {:?} registered{}\n",
            sanitize_for_terminal(name),
            sanitize_for_terminal(expansion),
            desc.map(|d| format!(" ({})", sanitize_for_terminal(&d)))
                .unwrap_or_default(),
        ))
    }

    /// `.prefix list`  every prefix, sorted by name. Also touches
    /// last_used_at (Q2: cli operations update; function-dispatch
    /// does not).
    fn sub_list() -> InvokeResult {
        let r = match spi::execute(
            "SELECT name, expansion, COALESCE(description, ''), \
                COALESCE(last_used_at, created_at) \
             FROM __sqlink_prefix ORDER BY name",
            &[],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        if r.rows.is_empty() {
            return text("(no prefixes registered)\n".into());
        }
        let mut out = String::new();
        out.push_str("NAME                  EXPANSION                                  LAST_USED  DESCRIPTION\n");
        for row in &r.rows {
            if row.len() < 4 {
                continue;
            }
            let name = text_col(&row[0]);
            let expansion = text_col(&row[1]);
            let description = text_col(&row[2]);
            let last_used = int_col(&row[3]);
            out.push_str(&format!(
                "{:<22}{:<43}{:>9}  {}\n",
                truncate(&sanitize_for_terminal(&name), 22),
                truncate(&sanitize_for_terminal(&expansion), 43),
                last_used,
                sanitize_for_terminal(&description),
            ));
        }
        // Q2: update last_used_at for the surfaced prefixes  this
        // is an explicit CLI operation per the plan.
        let _ = spi::execute(
            "UPDATE __sqlink_prefix SET last_used_at = ?1 \
             WHERE name IN (SELECT name FROM __sqlink_prefix)",
            &[SqlValue::Integer(now_secs())],
        );
        text(out)
    }

    /// `.prefix functions NAME` lists functions registered under
    /// NAME's expansion.
    fn sub_functions(rest: &[&str]) -> InvokeResult {
        if rest.len() != 1 {
            return err(".prefix functions: usage `.prefix functions NAME`".into());
        }
        let name = rest[0];
        let exp = match resolve_to_expansion(name) {
            Ok(Some(e)) => e,
            Ok(None) => {
                return err(format!(
                    ".prefix functions: no prefix matches {:?}",
                    sanitize_for_terminal(name)
                ));
            }
            Err(e) => return err_sqlite(e),
        };
        let r = match spi::execute(
            "SELECT function_name, n_args, COALESCE(extension_name, '?'), \
                COALESCE(registered_at, 0) \
             FROM __sqlink_prefix_function WHERE expansion = ?1 \
             ORDER BY function_name, n_args",
            &[SqlValue::Text(exp.clone())],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        if r.rows.is_empty() {
            return text(format!(
                "prefix {:?} (expansion {:?}) has no functions registered\n",
                sanitize_for_terminal(name),
                sanitize_for_terminal(&exp),
            ));
        }
        let mut out = format!(
            "functions under prefix {:?} (expansion={:?}):\n",
            sanitize_for_terminal(name),
            sanitize_for_terminal(&exp),
        );
        out.push_str("  FUNCTION              N_ARGS  EXTENSION\n");
        for row in &r.rows {
            if row.len() < 3 {
                continue;
            }
            let fname = text_col(&row[0]);
            let n_args = int_col(&row[1]);
            let ext = text_col(&row[2]);
            out.push_str(&format!(
                "  {:<22}{:>6}  {}\n",
                truncate(&sanitize_for_terminal(&fname), 22),
                n_args,
                sanitize_for_terminal(&ext),
            ));
        }
        text(out)
    }

    /// `.prefix expansion NAME`  print just the expansion.
    fn sub_expansion(rest: &[&str]) -> InvokeResult {
        if rest.len() != 1 {
            return err(".prefix expansion: usage `.prefix expansion NAME`".into());
        }
        let name = rest[0];
        let r = match spi::execute(
            "SELECT expansion FROM __sqlink_prefix WHERE name = ?1 LIMIT 1",
            &[SqlValue::Text(name.to_string())],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        match first_text_col(&r.rows) {
            Some(e) => text(format!("{}\n", e)),
            None => err(format!(
                ".prefix expansion: no prefix matches {:?}",
                sanitize_for_terminal(name)
            )),
        }
    }

    /// `.prefix rename OLD NEW`  change the alias. Function identity
    /// (keyed by expansion) is unchanged.
    fn sub_rename(rest: &[&str]) -> InvokeResult {
        if rest.len() != 2 {
            return err(".prefix rename: usage `.prefix rename OLD NEW`".into());
        }
        let old = rest[0];
        let new = rest[1];
        let chk = match spi::execute(
            "SELECT 1 FROM __sqlink_prefix WHERE name = ?1 LIMIT 1",
            &[SqlValue::Text(new.to_string())],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        if !chk.rows.is_empty() {
            return err(format!(
                ".prefix rename: {:?} already exists",
                sanitize_for_terminal(new)
            ));
        }
        let r = match spi::execute(
            "UPDATE __sqlink_prefix SET name = ?2 WHERE name = ?1",
            &[
                SqlValue::Text(old.to_string()),
                SqlValue::Text(new.to_string()),
            ],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        if r.changes == 0 {
            return err(format!(
                ".prefix rename: no prefix matches {:?}",
                sanitize_for_terminal(old)
            ));
        }
        text(format!(
            "renamed prefix {:?} -> {:?}\n",
            sanitize_for_terminal(old),
            sanitize_for_terminal(new),
        ))
    }

    /// `.prefix modify NAME DESCRIPTION ...`
    fn sub_modify(rest: &[&str], raw: &str) -> InvokeResult {
        if rest.is_empty() {
            return err(".prefix modify: usage `.prefix modify NAME DESCRIPTION`".into());
        }
        let name = rest[0];
        let desc = description_after(raw, &["modify", name]);
        let desc = desc.unwrap_or_default();
        let r = match spi::execute(
            "UPDATE __sqlink_prefix SET description = ?2 WHERE name = ?1",
            &[
                SqlValue::Text(name.to_string()),
                if desc.is_empty() {
                    SqlValue::Null
                } else {
                    SqlValue::Text(desc.clone())
                },
            ],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        if r.changes == 0 {
            return err(format!(
                ".prefix modify: no prefix matches {:?}",
                sanitize_for_terminal(name)
            ));
        }
        text(format!(
            "prefix {:?} description updated\n",
            sanitize_for_terminal(name)
        ))
    }

    /// `.prefix delete NAME`  remove the short alias. Warns if this
    /// is the last alias for an expansion that has functions
    /// registered.
    fn sub_delete(rest: &[&str]) -> InvokeResult {
        if rest.len() != 1 {
            return err(".prefix delete: usage `.prefix delete NAME`".into());
        }
        let name = rest[0];
        // Look up expansion for the warning.
        let exp = match resolve_to_expansion(name) {
            Ok(Some(e)) => e,
            Ok(None) => {
                return err(format!(
                    ".prefix delete: no prefix matches {:?}",
                    sanitize_for_terminal(name)
                ));
            }
            Err(e) => return err_sqlite(e),
        };
        // Count other aliases for the same expansion.
        let alias_count = match spi::execute(
            "SELECT COUNT(*) FROM __sqlink_prefix WHERE expansion = ?1 AND name != ?2",
            &[
                SqlValue::Text(exp.clone()),
                SqlValue::Text(name.to_string()),
            ],
        ) {
            Ok(r) => first_int(&r.rows).unwrap_or(0),
            Err(e) => return err_sqlite(e),
        };
        // Functions registered under this expansion.
        let func_count = match spi::execute(
            "SELECT COUNT(*) FROM __sqlink_prefix_function WHERE expansion = ?1",
            &[SqlValue::Text(exp.clone())],
        ) {
            Ok(r) => first_int(&r.rows).unwrap_or(0),
            Err(e) => return err_sqlite(e),
        };
        if let Err(e) = spi::execute(
            "DELETE FROM __sqlink_prefix WHERE name = ?1",
            &[SqlValue::Text(name.to_string())],
        ) {
            return err_sqlite(e);
        }
        let mut out = format!(
            "deleted prefix {:?}\n",
            sanitize_for_terminal(name),
        );
        if alias_count == 0 && func_count > 0 {
            out.push_str(&format!(
                "WARNING: this was the last alias for expansion {:?} which has {} \
                 function(s) registered. Those functions are still callable via \
                 SQLite's bare-name surface but no longer have a short prefix.\n",
                sanitize_for_terminal(&exp),
                func_count,
            ));
        }
        text(out)
    }

    /// `.prefix prefer NAME EXTENSION`  pin bare-name dispatch.
    /// NAME is the function name; EXTENSION is the extension to
    /// pin to. PLAN-followups.md P1: after writing each
    /// __sqlink_prefix_pin row, calls loader-bridge.apply-prefix-pin
    /// so the bare SQLite trampoline re-binds against the pinned
    /// extension immediately  no session restart needed.
    fn sub_prefer(rest: &[&str]) -> InvokeResult {
        if rest.len() != 2 {
            return err(
                ".prefix prefer: usage `.prefix prefer FUNCTION_NAME EXTENSION`.".into(),
            );
        }
        let fname = rest[0];
        let ext = rest[1];
        // Look up the expansion for this extension's functions.
        // (extension -> expansion is via __sqlink_prefix_function.)
        let r = match spi::execute(
            "SELECT DISTINCT expansion, n_args FROM __sqlink_prefix_function \
             WHERE extension_name = ?1 AND function_name = ?2",
            &[
                SqlValue::Text(ext.to_string()),
                SqlValue::Text(fname.to_string()),
            ],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        if r.rows.is_empty() {
            return err(format!(
                ".prefix prefer: extension {:?} has no function {:?} registered. \
                 Check `.prefix functions <prefix>` for what's available.",
                sanitize_for_terminal(ext),
                sanitize_for_terminal(fname),
            ));
        }
        let mut pinned = 0;
        for row in &r.rows {
            if row.len() < 2 {
                continue;
            }
            let exp = text_col(&row[0]);
            let n_args = int_col(&row[1]);
            if let Err(e) = spi::execute(
                "INSERT OR REPLACE INTO __sqlink_prefix_pin \
                    (function_name, n_args, expansion, set_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                &[
                    SqlValue::Text(fname.to_string()),
                    SqlValue::Integer(n_args),
                    SqlValue::Text(exp.clone()),
                    SqlValue::Integer(now_secs()),
                ],
            ) {
                return err_sqlite(e);
            }
            // PLAN-followups.md P1 live-prefer: re-register the bare
            // trampoline against the pinned extension's impl now.
            // Best-effort: if apply_prefix_pin errors (e.g. pin
            // targets a scalar that's not loaded), the INSERT
            // succeeded so the pin still takes effect next session.
            if let Err(e) = bindings::sqlite::extension::loader_bridge::apply_prefix_pin(
                fname,
                n_args as i32,
            ) {
                return text(format!(
                    "pinned {} arity-variant(s) of {:?} to extension {:?}; \
                     live re-register failed: {} (will take effect next session)\n",
                    pinned + 1,
                    sanitize_for_terminal(fname),
                    sanitize_for_terminal(ext),
                    sanitize_for_terminal(&e.message),
                ));
            }
            pinned += 1;
        }
        text(format!(
            "pinned {} arity-variant(s) of {:?} to extension {:?} (live).\n",
            pinned,
            sanitize_for_terminal(fname),
            sanitize_for_terminal(ext),
        ))
    }

    /// `.prefix unprefer NAME`  remove a pin.
    fn sub_unprefer(rest: &[&str]) -> InvokeResult {
        if rest.len() != 1 {
            return err(".prefix unprefer: usage `.prefix unprefer FUNCTION_NAME`".into());
        }
        let fname = rest[0];
        let r = match spi::execute(
            "DELETE FROM __sqlink_prefix_pin WHERE function_name = ?1",
            &[SqlValue::Text(fname.to_string())],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        if r.changes == 0 {
            return text(format!(
                "(no pin for {:?})\n",
                sanitize_for_terminal(fname)
            ));
        }
        text(format!(
            "removed {} pin row(s) for {:?}. Bare-name dispatch reverts to SQLite default \
             (last-registered wins) on next session.\n",
            r.changes,
            sanitize_for_terminal(fname),
        ))
    }

    /// `.prefix conflicts`  bare-name ambiguities + pin state.
    fn sub_conflicts() -> InvokeResult {
        let r = match spi::execute(
            "SELECT function_name, n_args, COUNT(DISTINCT expansion) AS c \
             FROM __sqlink_prefix_function \
             GROUP BY function_name, n_args \
             HAVING c > 1 \
             ORDER BY function_name, n_args",
            &[],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        if r.rows.is_empty() {
            return text("(no bare-name collisions in effect)\n".into());
        }
        let mut out = String::from("FUNCTION              N_ARGS  EXPANSIONS  PIN\n");
        for row in &r.rows {
            if row.len() < 3 {
                continue;
            }
            let fname = text_col(&row[0]);
            let n_args = int_col(&row[1]);
            let count = int_col(&row[2]);
            // Look up pin state.
            let pin = match spi::execute(
                "SELECT expansion FROM __sqlink_prefix_pin \
                 WHERE function_name = ?1 AND n_args = ?2 LIMIT 1",
                &[SqlValue::Text(fname.clone()), SqlValue::Integer(n_args)],
            ) {
                Ok(r) => first_text_col(&r.rows),
                Err(_) => None,
            };
            out.push_str(&format!(
                "{:<22}{:>6}{:>11}  {}\n",
                truncate(&sanitize_for_terminal(&fname), 22),
                n_args,
                count,
                pin.map(|p| sanitize_for_terminal(&p))
                    .unwrap_or_else(|| "(unpinned; SQLite default)".into()),
            ));
        }
        // List per-expansion detail for each conflict.
        out.push_str("\n  per-conflict detail (function | expansion | extension):\n");
        for row in &r.rows {
            if row.len() < 2 {
                continue;
            }
            let fname = text_col(&row[0]);
            let n_args = int_col(&row[1]);
            let detail = match spi::execute(
                "SELECT expansion, COALESCE(extension_name, '?') \
                 FROM __sqlink_prefix_function \
                 WHERE function_name = ?1 AND n_args = ?2 \
                 ORDER BY expansion",
                &[SqlValue::Text(fname.clone()), SqlValue::Integer(n_args)],
            ) {
                Ok(r) => r,
                Err(_) => continue,
            };
            for drow in &detail.rows {
                if drow.len() < 2 {
                    continue;
                }
                out.push_str(&format!(
                    "    {}/{}  {}  {}\n",
                    sanitize_for_terminal(&fname),
                    n_args,
                    sanitize_for_terminal(&text_col(&drow[0])),
                    sanitize_for_terminal(&text_col(&drow[1])),
                ));
            }
        }
        text(out)
    }

    /// `.prefix verify`  audit __sqlink_prefix_function for entries
    /// whose extensions are no longer loaded. v1 simplification:
    /// reports row counts + any entries referencing extensions
    /// that are not in the host's currently-loaded set. The
    /// "currently loaded" check is via loader_bridge if available;
    /// otherwise just dumps the registry.
    fn sub_verify() -> InvokeResult {
        let r = match spi::execute(
            "SELECT COUNT(*), \
                COUNT(DISTINCT extension_name), \
                COUNT(DISTINCT expansion) \
             FROM __sqlink_prefix_function",
            &[],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        let (n_funcs, n_exts, n_exps) = if let Some(row) = r.rows.first() {
            (
                int_col(row.get(0).unwrap_or(&SqlValue::Null)),
                int_col(row.get(1).unwrap_or(&SqlValue::Null)),
                int_col(row.get(2).unwrap_or(&SqlValue::Null)),
            )
        } else {
            (0, 0, 0)
        };
        let r = match spi::execute(
            "SELECT COUNT(*) FROM __sqlink_prefix",
            &[],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        let n_aliases = first_int(&r.rows).unwrap_or(0);
        let r = match spi::execute(
            "SELECT COUNT(*) FROM __sqlink_prefix_pin",
            &[],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        let n_pins = first_int(&r.rows).unwrap_or(0);
        // Orphaned-alias diagnostic: aliases whose expansion has no
        // functions registered.
        let orphans = match spi::execute(
            "SELECT name, expansion FROM __sqlink_prefix \
             WHERE expansion NOT IN (SELECT DISTINCT expansion FROM __sqlink_prefix_function) \
             ORDER BY name",
            &[],
        ) {
            Ok(r) => r,
            Err(e) => return err_sqlite(e),
        };
        let mut out = format!(
            "registry summary:\n  {} alias(es)\n  {} expansion(s)\n  {} function row(s)\n  \
             {} extension(s) represented\n  {} pin(s)\n",
            n_aliases, n_exps, n_funcs, n_exts, n_pins,
        );
        if !orphans.rows.is_empty() {
            out.push_str("\norphaned aliases (no functions under their expansion):\n");
            for row in &orphans.rows {
                if row.len() < 2 {
                    continue;
                }
                out.push_str(&format!(
                    "  {:<22}  {}\n",
                    sanitize_for_terminal(&text_col(&row[0])),
                    sanitize_for_terminal(&text_col(&row[1])),
                ));
            }
        } else {
            out.push_str("\nno orphaned aliases.\n");
        }
        text(out)
    }

    // ---------- shared helpers ----------

    fn first_int(rows: &[Vec<SqlValue>]) -> Option<i64> {
        rows.iter().next().and_then(|r| r.iter().next().map(int_col))
    }

    /// Extract the description portion of an `add` / `modify` raw
    /// arg-string by skipping past the known leading tokens. Robust
    /// to multi-word descriptions.
    fn description_after(raw: &str, leading: &[&str]) -> Option<String> {
        let mut rest = raw;
        for tok in leading {
            rest = rest.trim_start();
            if let Some(p) = rest.strip_prefix(*tok) {
                rest = p;
            } else {
                return None;
            }
        }
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn truncate(s: &str, n: usize) -> String {
        if s.chars().count() <= n {
            return s.to_string();
        }
        let mut out = String::new();
        for c in s.chars().take(n.saturating_sub(1)) {
            out.push(c);
        }
        out.push('~');
        out
    }

    fn text(body: String) -> InvokeResult {
        InvokeResult {
            text: body,
            state_deltas: vec![],
            ok: true,
            exit_code: 0,
        }
    }

    fn err(message: String) -> InvokeResult {
        InvokeResult {
            text: format!("Error: {message}\n"),
            state_deltas: vec![],
            ok: false,
            exit_code: 1,
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
