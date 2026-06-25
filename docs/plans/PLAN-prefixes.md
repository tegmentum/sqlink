# Plan: function prefixes — SPARQL-style namespacing for SQL functions

## Status (2026-06-25)

Design conversation captured. Not yet started. No substrate prereqs;
the loader-bridge already mediates function registration, so the
wrapper layer goes there. New `prefix-cli` extension joins the
cli-family.

## Motivation

As sqlink ranges across hundreds of extensions, naming collisions
between SQL functions are inevitable. Today SQLite's
`sqlite3_create_function` performs silent last-wins shadowing: if
two extensions both register `concat`, the second one transparently
replaces the first. Callers can't tell which implementation they
hit; the operator can't tell which extensions collide.

SPARQL solves the same problem for RDF identifiers with prefixed
names: a query declares `PREFIX foaf: <http://xmlns.com/foaf/0.1/>`
and then `foaf:name` is shorthand for the full URI. Two
independently-developed ontologies can both use the short `name`
inside their own namespace without colliding on the wire.

Sqlink prefixes adapt the same shape to SQL functions:

- Each extension declares a **short prefix** + a globally-unique
  **expansion** string.
- Functions are registered under their prefix; the SQL surface
  exposes both `name(...)` (bare) and `prefix__name(...)` (qualified)
  when the bare name is unambiguous, and only the qualified forms
  when two extensions collide.
- Calls to bare collided names error with a helpful "ambiguous;
  try `prefix__name` or `otherprefix__name`" message.

## Naming

The feature is called **prefixes**, not **namespaces**.
"Namespace" in SQLite already means an attached database schema
(`main`, `temp`, named attachments); reusing it would confuse two
unrelated concepts. SPARQL uses "prefix" and the metaphor maps
cleanly.

### Separator: `__` (double underscore)

`:` is the SPARQL-canonical separator but requires identifier
quoting in SQLite (`"foaf:name"(...)`) on every call. That's
high-friction.

`__` (double underscore) is the chosen separator. It is:
- Legal in unquoted SQLite identifiers (just `[A-Za-z_][A-Za-z0-9_]*`).
- Visually distinct from natural single-underscore names like
  `uuid_v4` or `json_extract`.
- ASCII-only — no encoding considerations.

Rejected alternatives:
- `:` — requires quoting everywhere; ergonomically poor.
- `.` — conflicts with SQL's `schema.function` syntax; parser
  ambiguity.
- `_` (single) — collides with naturally-named functions; ambiguous
  what's the prefix.
- `$` — works in some SQL dialects but tokenizes as one identifier
  in SQLite; less standard.
- `#` — non-identifier character; requires quoting.

## Architecture

### The expansion is required; the format is opaque

Each prefix has TWO required fields:
- `name` — the short prefix used in SQL (`foaf`, `tegmentum`).
- `expansion` — an opaque string that gives the prefix its global
  identity.

The expansion is unconstrained:
- A URL (`http://xmlns.com/foaf/0.1/`) — common, matches SPARQL.
- A Java-style namespace (`com.tegmentum.sqlink`).
- A UUID (`urn:uuid:...`).
- Any opaque string the extension author picks.

Sqlink does NOT validate the expansion's format. It is treated as
an opaque identity token.

### Function identity is (expansion, function_name)

The expansion — not the short prefix — is the canonical identity:

- The short prefix `foaf` is a per-database alias for the expansion
  `http://xmlns.com/foaf/0.1/`.
- Two databases can disagree on what `foaf` resolves to short-wise;
  `http://xmlns.com/foaf/0.1/name` is the same function everywhere.
- Renaming a prefix (`.prefix rename foaf bar`) is a SQL-syntax
  convenience that doesn't touch function identity in the registry.
- An extension's manifest declares both the preferred prefix and
  its expansion; if the prefix is already taken by a different
  expansion, the cli falls back to a numbered alternative (`foaf2`)
  or errors per operator policy.

### Storage (user database, `sqlite_sqlink_*` tables)

Two new tables, lived in the user database (the one the cli is
attached to). Per-db storage is the right choice because:
- Prefix mappings are configuration that should travel with the
  database (copying `db.sqlite` carries its prefix registrations).
- Different databases can have different prefix mappings without
  cross-talk.

The tables use the `sqlite_*` reserved-name convention so they
don't appear in casual `.tables` output:

```sql
CREATE TABLE sqlite_sqlink_prefix (
    name        TEXT PRIMARY KEY,    -- short prefix: 'foaf', 'tegmentum'
    expansion   TEXT NOT NULL,       -- opaque expanded form
    description TEXT,                -- optional human label
    created_at  INTEGER NOT NULL,
    last_used_at INTEGER             -- updated on function dispatch + .prefix touch
);
CREATE INDEX sqlite_sqlink_prefix_expansion
    ON sqlite_sqlink_prefix(expansion);

CREATE TABLE sqlite_sqlink_prefix_function (
    expansion      TEXT NOT NULL,    -- joins on expansion, NOT short name
    function_name  TEXT NOT NULL,    -- the bare name, e.g. 'name'
    extension_name TEXT,             -- which extension registered it (audit)
    n_args         INTEGER,          -- arity (or -1 for variadic)
    registered_at  INTEGER NOT NULL,
    PRIMARY KEY (expansion, function_name, n_args)
) WITHOUT ROWID;
```

Multiple prefixes can share an expansion (alias semantics). A
single `expansion` row in `_prefix_function` is canonical; multiple
short-name aliases in `_prefix` may reference it.

### Conflict resolution: both-explicit

When extension A's `concat` (expansion `com.exta.tools`) and
extension B's `concat` (expansion `org.extb.lib`) are both
registered:

1. Both become individually callable: `exta__concat(...)` and
   `extb__concat(...)`.
2. The bare name `concat(...)` is NOT registered with SQLite.
3. A SQL call to `concat(...)` errors with SQLite's standard
   "no such function" error.
4. The cli intercepts that error and surfaces:
   ```
   no such function: concat
   ambiguous between:
     exta__concat (extension exta, expansion com.exta.tools)
     extb__concat (extension extb, expansion org.extb.lib)
   ```

When extension A registers `uuid_v4` and no other extension has
that name:

1. `uuid_v4(...)` is registered normally as the bare name.
2. `exta__uuid_v4(...)` is ALSO registered (always-available
   qualified form).

So callers always have access to the qualified form, but only get
the bare form when it's unambiguous.

### Registration flow (in the loader-bridge wrapper)

When an extension registers a scalar/aggregate/vtab function
through `spi-loader.register-scalar` etc., the host's loader-bridge
wrapper:

1. Reads the extension's manifest for `(preferred-prefix, expansion)`.
   If absent, error: extensions must declare a prefix.
2. Looks up the prefix in `sqlite_sqlink_prefix`:
   - If exists with matching expansion → use it.
   - If exists with different expansion → fall back to numbered
     alternative (`foaf2`, `foaf3`, ...) OR error per operator
     policy. v1 default: fall back + warn; operator can
     `.prefix rename` after.
   - If absent → insert it.
3. Inserts the function into `sqlite_sqlink_prefix_function` keyed
   by `(expansion, function_name, n_args)`.
4. Registers the function with SQLite under `prefix__function_name`
   (always).
5. If no other expansion has a function named `function_name` with
   the same arity, ALSO registers the bare `function_name`.
6. If a SECOND extension later registers a function with the same
   bare name + arity, deregister the bare name (it becomes
   ambiguous from that point forward) and update `_prefix_function`
   for both.

## Surface

### Dot commands (in `prefix-cli` extension)

```sql
-- register a prefix; expansion required
.prefix add foaf http://xmlns.com/foaf/0.1/ "Friend of a friend ontology"
.prefix add tegmentum com.tegmentum.sqlink
.prefix add my opaque-token-i-want

-- inspect
.prefix list                       -- name | expansion | description | last_used
.prefix functions foaf             -- functions registered under foaf's expansion
.prefix expansion foaf             -- print just the expansion string

-- modify
.prefix rename foaf bar            -- change the short alias; expansion + functions unchanged
.prefix modify foaf "Updated description"

-- prune
.prefix delete foaf                -- removes the alias row; expansion-based
                                   -- _prefix_function entries persist (other
                                   -- aliases for the same expansion still work)

-- diagnostics
.prefix conflicts                  -- bare-name ambiguities currently in effect
.prefix verify                     -- check that every _prefix_function row
                                   -- has an extension still loaded; warn on
                                   -- stale entries
```

### Extension manifest declaration

```toml
[package.metadata.extension]
preferred-prefix = "foaf"
prefix-expansion = "http://xmlns.com/foaf/0.1/"
```

Both fields required. Loader rejects extensions without both.

For existing extensions that don't yet declare a prefix, v1 ships a
short migration window: the loader emits a deprecation warning +
auto-assigns the extension's crate name as the prefix and a
synthetic expansion (`sqlink-internal://crate-name`). The migration
ends in v1.1; the synthetic-expansion path becomes an error.

### Function call resolution

| Call | Behavior |
|---|---|
| `foaf__name(...)` | Always works if `(foaf's expansion, "name")` is registered. |
| `name(...)` (unique) | Works if exactly one expansion has `name` at that arity. |
| `name(...)` (ambiguous) | `no such function: name` from SQLite + cli-side disambiguation hint. |
| `unknown__name(...)` | `no such function: unknown__name` — short prefix unknown. |

### Capability requirements

| Surface | Capability needed |
|---|---|
| `.prefix add` / `delete` / `rename` / `modify` | `Spi` (writes user db) |
| `.prefix list` / `functions` / `expansion` / `conflicts` / `verify` | `Spi` (read-only) |
| Extension's own prefix registration on load | None new; happens host-side during the loader's existing capability check |

A new `Capability::PrefixRegistry` was considered but rejected for
v1: the prefix tables live in the user db and read/write goes
through `spi.execute`. The existing Spi capability gate suffices.

## v1 scope

- The two tables + their migration into existing dbs.
- Loader-bridge wrapper around scalar / aggregate / vtab
  registration.
- `prefix-cli` extension with the six dot-commands above.
- Both-explicit collision resolution.
- Manifest-driven prefix declaration with deprecation-period
  fallback for un-declared extensions.
- Tests covering: bare-name happy path, ambiguity path, qualified-
  form fallback, rename semantics, delete-with-shared-expansion,
  manifest-missing-deprecation-warning, registration of all 4
  function shapes (scalar / aggregate / vtab / hook).
- Docs + README section.

## Out of scope (v2+)

- **Per-query prefix overrides**: a syntax like `SELECT prefix
  foaf=other; foaf__name(...)` to use a different expansion for
  one query. SPARQL has this; SQL doesn't natively. Could land as
  a session pragma later.
- **Prefix lock-in**: a policy bit per prefix that prevents
  renaming once functions are registered. Operator hygiene; not
  urgent.
- **Prefix-scoped permissions**: gate per-prefix at the capability
  layer ("only operator can use `system__*`"). Useful for trusted
  vs untrusted extension separation; needs more design.
- **Bulk import/export**: `.prefix export foaf > foaf.toml` /
  `.prefix import foaf.toml` for sharing prefix sets across dbs.
  Easy to add; not v1.
- **Auto-suggestion on typos**: `concat(...)` errors could suggest
  `levenshtein-closest` qualified forms. Nice UX; not v1.
- **Cross-database prefix sync**: replicate prefix registrations
  across multiple attached dbs. v2.
- **Prefix registry hosting**: a tegmentum-org-level (or wider)
  registry where extensions publish their `(prefix, expansion)`
  pair so the loader can verify globally-unique expansions. Way
  out of scope.

## Effort estimate

| Piece | Effort |
|---|---|
| Schema additions + migration into existing dbs | 0.5 day |
| Loader-bridge wrapper: registration with disambiguation | 1 day |
| Manifest field parsing + deprecation-window fallback | 0.5 day |
| `prefix-cli` extension scaffold + six dot-commands | 1 day |
| Function dispatch + bare-name shadowing on collision | 1 day |
| Native integration tests (round-trip + collision + rename) | 1 day |
| Browser smoke + docs | 0.5 day |

**Total: ~5.5 days for v1.**

## Dependencies

- `spi.execute` against the user db (already exists).
- `spi-loader.register-scalar`/`register-aggregate`/`register-vtab`/
  `register-hook` — already exists; this plan wraps them.
- The loader-bridge's manifest-reading code (already exists).

No new substrate / capability variants required.

## Sequencing

1. Land **schema + migration** (`sqlite_sqlink_prefix` +
   `sqlite_sqlink_prefix_function`). Small standalone commit.
2. Land **manifest parsing** for `preferred-prefix` +
   `prefix-expansion`. Touches loader's manifest types.
3. Land **registration wrapper** in the loader-bridge that
   inserts/queries the new tables + decides bare-vs-qualified
   registration.
4. Land **prefix-cli extension** with the six dot-commands.
5. **Native integration tests** covering happy path + collision +
   rename.
6. **Browser smoke** (metadata-only commands; registration happens
   at extension-load time in browser too).
7. **Deprecation-period extension audit**: scan all built-in
   extensions, emit warnings for those missing the manifest
   declaration, propose `(prefix, expansion)` pairs for each.

## Open questions

1. **Auto-fallback on prefix collision: numbered alternative or
   error?** When extension A loads with `foaf` → `expansion-A` and
   extension B later loads claiming `foaf` → `expansion-B`,
   should the loader (a) auto-assign extension B the next free
   `foaf2`, (b) refuse to load extension B, or (c) prompt the
   operator? v1 default: (a) with a warning. Operator can
   `.prefix rename` post-load.

2. **Should bare-name dispatch update `last_used_at`?** Trivial
   write per function call; useful for `.prefix gc` later, but adds
   write traffic per scalar invocation. v1 default: NO update on
   call; only update on `.prefix` cli operations + `.prefix verify`.

3. **What's the deprecation window for extensions missing the
   manifest declaration?** v1 warns + auto-assigns. When does the
   warning escalate to a hard error? Suggest tying to a v1.x
   release rather than calendar time.

4. **Should the loader-bridge wrapper apply to ALL function shapes
   uniformly (scalar / aggregate / vtab / hook)** or just to
   scalars + aggregates? Vtabs and hooks have a different namespace
   in SQLite (vtab is `CREATE VIRTUAL TABLE`; hook is
   `sqlite3_create_window_function` / collation / etc.). v1
   recommendation: scalar + aggregate (the common collision
   surface); vtab + hook deferred to v1.1.

## References

- SPARQL 1.1 Query Language, §4 "RDF Term Syntax" (prefixed names).
- SQLite identifier syntax (`https://sqlite.org/lang_keywords.html`).
- PLAN-bundles.md (sibling plan; same dispatch-bridge + manifest
  pattern this plan extends).
