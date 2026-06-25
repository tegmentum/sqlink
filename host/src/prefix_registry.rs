//! Function-prefix namespacing registry (PLAN-prefixes.md).
//!
//! Three tables on the user database, idempotently installed:
//!
//!   __sqlink_prefix             short prefix -> opaque expansion
//!                                    (e.g. 'foaf' -> 'http://xmlns.com/foaf/0.1/')
//!   __sqlink_prefix_function    (expansion, function_name, n_args) ->
//!                                    which extension provides it
//!   __sqlink_prefix_pin         (function_name, n_args) -> pinned
//!                                    expansion (operator-controlled bare-name
//!                                    dispatch on collision; v1.1 surface)
//!
//! Registration policy (Q5 of PLAN-prefixes.md, strictly additive):
//!
//!   1. Bare `name` is ALWAYS registered with SQLite. Last-registered
//!      wins for that name+arity, matching SQLite's existing semantics.
//!      No user-visible behavior change.
//!   2. Qualified `prefix__name` is ALSO registered, every time, for
//!      every function. Callers always have an explicit dispatch path.
//!   3. If `_prefix_pin` has a row for `(name, n_args)` and the pin
//!      targets a DIFFERENT expansion than the extension currently
//!      registering, the bare name is skipped for this extension
//!      (the pinned expansion's load registered/will register the
//!      bare form).
//!   4. Collisions (multiple expansions sharing the same `(name,
//!      n_args)`) emit a load-time warning. Operator runs
//!      `.prefix conflicts` to inspect.

use anyhow::{anyhow, Result};
use sqlite_component_core::db::{Connection, StepResult, Value};

/// SQLite identifier separator between prefix and function name.
/// Chosen `__` because it's legal in unquoted SQLite identifiers
/// and visually distinct from natural single-underscore names like
/// `uuid_v4`. Rejected alternatives: `:` (quoting overhead), `.`
/// (parser ambiguity with schema.function), `_` (collides with
/// natural names), `$`/`#` (non-portable).
pub const PREFIX_SEPARATOR: &str = "__";

/// Bound on auto-fallback `foafN` collision resolution. After
/// 999 collisions on the same short prefix the loader gives up.
pub const COLLISION_FALLBACK_LIMIT: u32 = 999;

/// Idempotent schema DDL. Safe to run on every cli session start.
pub const SCHEMA_DDL: &str = "\
CREATE TABLE IF NOT EXISTS __sqlink_prefix (
    name         TEXT PRIMARY KEY,
    expansion    TEXT NOT NULL,
    description  TEXT,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER
);
CREATE INDEX IF NOT EXISTS __sqlink_prefix_expansion
    ON __sqlink_prefix(expansion);
CREATE TABLE IF NOT EXISTS __sqlink_prefix_function (
    expansion      TEXT NOT NULL,
    function_name  TEXT NOT NULL,
    extension_name TEXT,
    n_args         INTEGER NOT NULL,
    registered_at  INTEGER NOT NULL,
    PRIMARY KEY (expansion, function_name, n_args)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS __sqlink_prefix_pin (
    function_name TEXT NOT NULL,
    n_args        INTEGER NOT NULL,
    expansion     TEXT NOT NULL,
    set_at        INTEGER NOT NULL,
    PRIMARY KEY (function_name, n_args)
) WITHOUT ROWID;
";

/// Install the three prefix-registry tables on the connection.
/// Uses `CREATE TABLE IF NOT EXISTS` so safe across reopens +
/// reloads. Treats failures as non-fatal so an in-memory or
/// read-only db that can't host the tables doesn't kill loading.
pub fn install_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_DDL)
        .map_err(|e| anyhow!("install prefix-registry schema: {}", e.message))
}

/// Resolve an extension's `(prefix, expansion, is_synthetic)` for
/// registration. Implements the Q3 deprecation-window fallback:
///
///   - If both `preferred_prefix` and `prefix_expansion` are
///     present in the manifest, use them directly.
///   - Otherwise, emit a tracing warning and synthesize:
///       prefix    = sanitize(crate_name)
///       expansion = "sqlink-internal://" + crate_name
///   - The `is_synthetic` flag tells callers whether the
///     extension is running on the deprecation-window fallback
///     (so the cli can surface a roll-up warning at the end of
///     a load session, etc.).
pub fn resolve_prefix_expansion(
    crate_name: &str,
    preferred_prefix: Option<&str>,
    prefix_expansion: Option<&str>,
) -> (String, String, bool) {
    match (preferred_prefix, prefix_expansion) {
        (Some(p), Some(e)) if !p.is_empty() && !e.is_empty() => {
            (p.to_string(), e.to_string(), false)
        }
        _ => {
            let prefix = sanitize_prefix(crate_name);
            let expansion = format!("sqlink-internal://{crate_name}");
            tracing::warn!(
                extension = crate_name,
                synthetic_prefix = %prefix,
                synthetic_expansion = %expansion,
                "extension missing preferred-prefix/prefix-expansion in manifest; \
                 using synthetic. This will be a hard error in v1.1."
            );
            (prefix, expansion, true)
        }
    }
}

/// Sanitize a crate name into a legal unquoted SQLite identifier.
/// Replaces any char outside `[A-Za-z0-9_]` with `_`. If the
/// result starts with a digit, prepends `_`.
fn sanitize_prefix(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
        if i == 0 && c.is_ascii_digit() {
            out.insert(0, '_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Build the qualified function name: `prefix__name`.
pub fn qualify(prefix: &str, name: &str) -> String {
    format!("{prefix}{PREFIX_SEPARATOR}{name}")
}

/// Insert or update the `_prefix` row binding `prefix` to `expansion`.
/// Returns the actual prefix to use (may differ from input on
/// Q1 collision fallback).
///
/// Q1 policy: if `prefix` is already bound to a DIFFERENT expansion,
/// try `prefix2`, `prefix3`, ... up to `prefix999`. Emit a
/// tracing warning naming both expansions + the auto-assigned alias.
pub fn record_prefix_with_collision_fallback(
    conn: &Connection,
    requested_prefix: &str,
    expansion: &str,
    now: i64,
) -> Result<String> {
    // Fast path: the prefix is unbound, or already bound to this
    // exact expansion (idempotent reload).
    let existing = lookup_expansion(conn, requested_prefix)?;
    if existing.as_deref() == Some(expansion) {
        return Ok(requested_prefix.to_string());
    }
    if existing.is_none() {
        insert_prefix(conn, requested_prefix, expansion, now)?;
        return Ok(requested_prefix.to_string());
    }
    // Collision: existing prefix is bound to a different expansion.
    // Walk prefix2..prefix999 looking for a free slot.
    let existing_expansion = existing.unwrap();
    for n in 2..=COLLISION_FALLBACK_LIMIT {
        let alias = format!("{requested_prefix}{n}");
        let alias_existing = lookup_expansion(conn, &alias)?;
        match alias_existing {
            Some(e) if e == expansion => {
                // Same expansion already at the fallback slot; reuse.
                return Ok(alias);
            }
            Some(_) => continue,
            None => {
                insert_prefix(conn, &alias, expansion, now)?;
                tracing::warn!(
                    requested_prefix = requested_prefix,
                    existing_expansion = %existing_expansion,
                    new_expansion = expansion,
                    assigned_alias = %alias,
                    "prefix collision: short prefix already bound to a different \
                     expansion; auto-assigning numbered alternative. Use \
                     `.prefix rename` to override."
                );
                return Ok(alias);
            }
        }
    }
    Err(anyhow!(
        "prefix collision: '{}' and its {}-deep numbered alternatives are all \
         bound to different expansions",
        requested_prefix,
        COLLISION_FALLBACK_LIMIT
    ))
}

fn lookup_expansion(conn: &Connection, prefix: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT expansion FROM __sqlink_prefix WHERE name = ?1")
        .map_err(|e| anyhow!("prepare lookup_expansion: {}", e.message))?;
    stmt.bind(1, &Value::Text(prefix.to_string()))
        .map_err(|e| anyhow!("bind lookup_expansion: {}", e.message))?;
    match stmt
        .step()
        .map_err(|e| anyhow!("step lookup_expansion: {}", e.message))?
    {
        StepResult::Row => match stmt.column_value(0) {
            Value::Text(s) => Ok(Some(s)),
            _ => Ok(None),
        },
        StepResult::Done => Ok(None),
    }
}

fn insert_prefix(conn: &Connection, prefix: &str, expansion: &str, now: i64) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "INSERT INTO __sqlink_prefix(name, expansion, created_at) \
             VALUES (?1, ?2, ?3)",
        )
        .map_err(|e| anyhow!("prepare insert_prefix: {}", e.message))?;
    stmt.bind_all(&[
        Value::Text(prefix.to_string()),
        Value::Text(expansion.to_string()),
        Value::Integer(now),
    ])
    .map_err(|e| anyhow!("bind insert_prefix: {}", e.message))?;
    stmt.step()
        .map_err(|e| anyhow!("step insert_prefix: {}", e.message))?;
    Ok(())
}

/// Record a function registration into `_prefix_function`. Returns
/// the list of OTHER expansions that have a function with the same
/// `(name, n_args)` already registered — callers use this to log
/// collision warnings.
pub fn record_function(
    conn: &Connection,
    expansion: &str,
    function_name: &str,
    n_args: i32,
    extension_name: &str,
    now: i64,
) -> Result<Vec<String>> {
    // Find any pre-existing rows from other expansions before
    // inserting (we don't want to detect ourselves).
    let mut other_expansions = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT expansion FROM __sqlink_prefix_function \
                 WHERE function_name = ?1 AND n_args = ?2 AND expansion <> ?3",
            )
            .map_err(|e| anyhow!("prepare collision-scan: {}", e.message))?;
        stmt.bind_all(&[
            Value::Text(function_name.to_string()),
            Value::Integer(n_args as i64),
            Value::Text(expansion.to_string()),
        ])
        .map_err(|e| anyhow!("bind collision-scan: {}", e.message))?;
        loop {
            match stmt
                .step()
                .map_err(|e| anyhow!("step collision-scan: {}", e.message))?
            {
                StepResult::Row => {
                    if let Value::Text(s) = stmt.column_value(0) {
                        other_expansions.push(s);
                    }
                }
                StepResult::Done => break,
            }
        }
    }
    // Upsert: REPLACE on PRIMARY KEY conflict so re-registering the
    // same (expansion, name, n_args) just refreshes registered_at +
    // extension_name (e.g. on reload).
    let mut stmt = conn
        .prepare(
            "INSERT INTO __sqlink_prefix_function \
             (expansion, function_name, extension_name, n_args, registered_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(expansion, function_name, n_args) DO UPDATE SET \
                extension_name = excluded.extension_name, \
                registered_at  = excluded.registered_at",
        )
        .map_err(|e| anyhow!("prepare record_function: {}", e.message))?;
    stmt.bind_all(&[
        Value::Text(expansion.to_string()),
        Value::Text(function_name.to_string()),
        Value::Text(extension_name.to_string()),
        Value::Integer(n_args as i64),
        Value::Integer(now),
    ])
    .map_err(|e| anyhow!("bind record_function: {}", e.message))?;
    stmt.step()
        .map_err(|e| anyhow!("step record_function: {}", e.message))?;
    Ok(other_expansions)
}

/// Look up a pin for `(function_name, n_args)`. Returns the pinned
/// expansion if a row exists, else None. Pins are operator-set via
/// `.prefix prefer` (v1.1 surface).
pub fn lookup_pin(
    conn: &Connection,
    function_name: &str,
    n_args: i32,
) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT expansion FROM __sqlink_prefix_pin \
             WHERE function_name = ?1 AND n_args = ?2",
        )
        .map_err(|e| anyhow!("prepare lookup_pin: {}", e.message))?;
    stmt.bind_all(&[
        Value::Text(function_name.to_string()),
        Value::Integer(n_args as i64),
    ])
    .map_err(|e| anyhow!("bind lookup_pin: {}", e.message))?;
    match stmt
        .step()
        .map_err(|e| anyhow!("step lookup_pin: {}", e.message))?
    {
        StepResult::Row => match stmt.column_value(0) {
            Value::Text(s) => Ok(Some(s)),
            _ => Ok(None),
        },
        StepResult::Done => Ok(None),
    }
}

/// Decide whether THIS extension's registration should claim the
/// bare name `function_name(n_args)`, given the pin state:
///
///   - No pin → yes (bare gets last-wins semantics; matches SQLite default).
///   - Pin targets THIS extension's expansion → yes.
///   - Pin targets a DIFFERENT expansion → no, skip bare registration.
pub fn should_register_bare(
    conn: &Connection,
    function_name: &str,
    n_args: i32,
    my_expansion: &str,
) -> Result<bool> {
    match lookup_pin(conn, function_name, n_args)? {
        None => Ok(true),
        Some(pinned) => Ok(pinned == my_expansion),
    }
}

/// Emit a tracing warning describing a function-name collision.
/// Called when `record_function` reports >0 other expansions for
/// the same `(name, n_args)`.
pub fn warn_function_collision(
    function_name: &str,
    n_args: i32,
    my_extension: &str,
    my_expansion: &str,
    my_prefix: &str,
    other_expansions: &[String],
    bare_owner: BareNameOwner,
) {
    let qualified = qualify(my_prefix, function_name);
    let bare_text = match bare_owner {
        BareNameOwner::ThisExtension => format!(
            "bare `{function_name}` dispatches to {my_extension} (last-wins). \
             Use `.prefix prefer {function_name} <ext>` to pin a different impl."
        ),
        BareNameOwner::PinnedElsewhere(p) => format!(
            "bare `{function_name}` is pinned to expansion '{p}'; this extension's \
             bare-name registration was skipped. Call `{qualified}` for explicit \
             dispatch."
        ),
    };
    tracing::warn!(
        function = function_name,
        n_args,
        my_extension,
        my_expansion,
        other_expansions = ?other_expansions,
        "function collision: {} also register `{}/{}`. {}",
        if other_expansions.len() == 1 {
            "another extension does"
        } else {
            "other extensions"
        },
        function_name,
        n_args,
        bare_text
    );
}

/// Whether the calling extension claims the bare name in this
/// load — used to format the collision warning.
pub enum BareNameOwner {
    ThisExtension,
    PinnedElsewhere(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlite_component_core::db::OpenFlags;

    fn fresh() -> Connection {
        let conn = Connection::open(":memory:", OpenFlags::DEFAULT).unwrap();
        install_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn schema_installs_idempotently() {
        let conn = fresh();
        // Run again; must not error.
        install_schema(&conn).unwrap();
    }

    #[test]
    fn sanitize_prefix_rejects_non_identifier_chars() {
        assert_eq!(sanitize_prefix("foaf"), "foaf");
        assert_eq!(sanitize_prefix("foo-bar"), "foo_bar");
        assert_eq!(sanitize_prefix("foo.bar"), "foo_bar");
        assert_eq!(sanitize_prefix("foo:bar"), "foo_bar");
        assert_eq!(sanitize_prefix("3foo"), "_3foo");
        assert_eq!(sanitize_prefix(""), "_");
    }

    #[test]
    fn resolve_uses_manifest_when_both_present() {
        let (p, e, s) = resolve_prefix_expansion(
            "uuid",
            Some("uuid"),
            Some("urn:uuid"),
        );
        assert_eq!(p, "uuid");
        assert_eq!(e, "urn:uuid");
        assert!(!s);
    }

    #[test]
    fn resolve_synthesizes_when_missing() {
        let (p, e, s) = resolve_prefix_expansion("foo-bar", None, None);
        assert_eq!(p, "foo_bar");
        assert_eq!(e, "sqlink-internal://foo-bar");
        assert!(s);
    }

    #[test]
    fn resolve_synthesizes_when_one_missing() {
        // Manifest with only prefix and no expansion: treat as
        // incomplete and synthesize both.
        let (p, e, s) = resolve_prefix_expansion("uuid", Some("uuid"), None);
        assert_eq!(p, "uuid");
        assert_eq!(e, "sqlink-internal://uuid");
        assert!(s);
    }

    #[test]
    fn record_prefix_idempotent_on_same_expansion() {
        let conn = fresh();
        let p1 = record_prefix_with_collision_fallback(&conn, "foaf", "http://example/foaf", 100)
            .unwrap();
        let p2 = record_prefix_with_collision_fallback(&conn, "foaf", "http://example/foaf", 200)
            .unwrap();
        assert_eq!(p1, "foaf");
        assert_eq!(p2, "foaf");
    }

    #[test]
    fn record_prefix_falls_back_to_numbered_alias_on_collision() {
        let conn = fresh();
        let p1 = record_prefix_with_collision_fallback(&conn, "foaf", "http://example/foaf-a", 100)
            .unwrap();
        let p2 = record_prefix_with_collision_fallback(&conn, "foaf", "http://example/foaf-b", 200)
            .unwrap();
        assert_eq!(p1, "foaf");
        assert_eq!(p2, "foaf2");
        // And a third with a different expansion goes to foaf3.
        let p3 = record_prefix_with_collision_fallback(&conn, "foaf", "http://example/foaf-c", 300)
            .unwrap();
        assert_eq!(p3, "foaf3");
    }

    #[test]
    fn record_prefix_collision_reuses_existing_fallback_slot() {
        // foaf -> A, foaf2 -> B already exist. Asking for foaf -> B
        // should return foaf2 (the existing slot), not create foaf3.
        let conn = fresh();
        record_prefix_with_collision_fallback(&conn, "foaf", "http://example/foaf-a", 100).unwrap();
        record_prefix_with_collision_fallback(&conn, "foaf", "http://example/foaf-b", 200).unwrap();
        let p3 = record_prefix_with_collision_fallback(&conn, "foaf", "http://example/foaf-b", 300)
            .unwrap();
        assert_eq!(p3, "foaf2");
    }

    #[test]
    fn record_function_detects_collisions() {
        let conn = fresh();
        record_prefix_with_collision_fallback(&conn, "exta", "exp-a", 100).unwrap();
        record_prefix_with_collision_fallback(&conn, "extb", "exp-b", 100).unwrap();
        let coll_a = record_function(&conn, "exp-a", "concat", 2, "exta", 100).unwrap();
        assert!(coll_a.is_empty());
        let coll_b = record_function(&conn, "exp-b", "concat", 2, "extb", 100).unwrap();
        assert_eq!(coll_b, vec!["exp-a".to_string()]);
    }

    #[test]
    fn record_function_idempotent_on_reload() {
        let conn = fresh();
        record_prefix_with_collision_fallback(&conn, "foaf", "exp", 100).unwrap();
        let c1 = record_function(&conn, "exp", "name", 1, "ext", 100).unwrap();
        let c2 = record_function(&conn, "exp", "name", 1, "ext", 200).unwrap();
        assert!(c1.is_empty());
        // Same expansion re-registering doesn't see itself as a collision.
        assert!(c2.is_empty());
    }

    #[test]
    fn should_register_bare_with_no_pin_is_true() {
        let conn = fresh();
        assert!(should_register_bare(&conn, "concat", 2, "any").unwrap());
    }

    #[test]
    fn should_register_bare_respects_pin() {
        let conn = fresh();
        // Insert a pin pointing at expansion "exp-a".
        let mut stmt = conn
            .prepare(
                "INSERT INTO __sqlink_prefix_pin \
                 (function_name, n_args, expansion, set_at) \
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .unwrap();
        stmt.bind_all(&[
            Value::Text("concat".into()),
            Value::Integer(2),
            Value::Text("exp-a".into()),
            Value::Integer(100),
        ])
        .unwrap();
        stmt.step().unwrap();
        assert!(should_register_bare(&conn, "concat", 2, "exp-a").unwrap());
        assert!(!should_register_bare(&conn, "concat", 2, "exp-b").unwrap());
    }

    #[test]
    fn qualify_uses_double_underscore() {
        assert_eq!(qualify("foaf", "name"), "foaf__name");
        assert_eq!(qualify("uuid", "v4"), "uuid__v4");
    }
}
