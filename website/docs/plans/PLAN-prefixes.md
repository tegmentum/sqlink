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

The tables use the `__sqlink_*` double-underscore prefix
convention (matching the cas-cache's `__cas_*` tables). SQLite
itself reserves names starting with `sqlite_` for internal use
and rejects user-CREATE of such tables; the `__` prefix is the
project's analog for hiding internal bookkeeping from casual
`.tables` output:

```sql
CREATE TABLE __sqlink_prefix (
    name        TEXT PRIMARY KEY,    -- short prefix: 'foaf', 'tegmentum'
    expansion   TEXT NOT NULL,       -- opaque expanded form
    description TEXT,                -- optional human label
    created_at  INTEGER NOT NULL,
    last_used_at INTEGER             -- updated on function dispatch + .prefix touch
);
CREATE INDEX __sqlink_prefix_expansion
    ON __sqlink_prefix(expansion);

CREATE TABLE __sqlink_prefix_function (
    expansion      TEXT NOT NULL,    -- joins on expansion, NOT short name
    function_name  TEXT NOT NULL,    -- the bare name, e.g. 'name'
    extension_name TEXT,             -- which extension registered it (audit)
    n_args         INTEGER,          -- arity (or -1 for variadic)
    registered_at  INTEGER NOT NULL,
    PRIMARY KEY (expansion, function_name, n_args)
) WITHOUT ROWID;

-- Optional operator-set pin: when multiple extensions register the
-- same bare name + arity, this row says which one wins the bare
-- name dispatch. NULL pin means "follow SQLite default" (last-
-- registered wins).
CREATE TABLE __sqlink_prefix_pin (
    function_name TEXT NOT NULL,
    n_args        INTEGER NOT NULL,
    expansion     TEXT NOT NULL,     -- which expansion's impl wins bare-name
    set_at        INTEGER NOT NULL,
    PRIMARY KEY (function_name, n_args)
) WITHOUT ROWID;
```

Multiple prefixes can share an expansion (alias semantics). A
single `expansion` row in `_prefix_function` is canonical; multiple
short-name aliases in `_prefix` may reference it.

### Conflict resolution: bare name preserves existing behavior; qualified forms are purely additive

**Hard constraint**: existing user SQL must not break. Users today
write `SELECT uuid_v4()` without any prefix; that has to keep working
exactly the same way after this feature lands, regardless of how
many extensions are loaded or whether any of them collide on the
bare name.

Three cases:

**Case 1 — no collision.** Extension A registers `uuid_v4`; no
other extension has that name at the same arity:
1. `uuid_v4(...)` is registered as the bare name (visible to all
   existing SQL).
2. `exta__uuid_v4(...)` is ALSO registered (always-available
   qualified form, for operators who want explicit dispatch).

**Case 2 — collision, bare name preserved.** Extension A's `concat`
(expansion `com.exta.tools`) is registered first. Extension B's
`concat` (expansion `org.extb.lib`) loads later:
1. The bare name `concat(...)` continues to follow SQLite's
   default behavior — whichever extension's registration is current
   in SQLite's function table wins. By default this is the
   **last-registered** (B in this example), matching SQLite's
   existing semantics. Existing SQL that calls `concat(...)` keeps
   working; the IMPL it gets is whatever it would have gotten
   without this feature.
2. Both qualified forms are added: `exta__concat(...)` AND
   `extb__concat(...)`. Always callable; always unambiguous.
3. The cli logs a load-time warning: "function `concat/N` registered
   by both `exta` (expansion=com.exta.tools) and `extb`
   (expansion=org.extb.lib); bare call dispatches to `extb`. Use
   `.prefix conflicts` to inspect; `exta__concat` and
   `extb__concat` are available for explicit dispatch."
4. Operator can `.prefix prefer concat exta` to pin the bare name
   to a specific extension. This writes a row to
   `__sqlink_prefix_pin` and re-registers the bare name
   against the pinned extension's implementation.

**Case 3 — extension unloaded.** Extension B unloads (releasing its
`concat` registration). The bare name reverts to extension A's
implementation (the remaining registration). Qualified
`exta__concat` continues to work; `extb__concat` is no longer
callable (extension B is gone).

### What this feature is NOT

- It does NOT change which implementation `concat()` dispatches to
  when called bare. SQLite's default semantics + the operator's
  optional `.prefix prefer` decide that.
- It does NOT error on ambiguity at call time. Bare-name calls
  always work as long as ANY extension has registered that name.
- It does NOT require users to update existing SQL. The only new
  syntax (`prefix__name`) is opt-in for callers who want explicit
  dispatch.

### What this feature IS

- Always-available qualified forms (`prefix__name`) so a SQL
  caller CAN unambiguously target a specific implementation when
  they need to.
- Load-time warnings + a `.prefix conflicts` view so operators
  can SEE collisions that would otherwise be silent.
- An optional `.prefix prefer` pin so operators can control which
  implementation wins the bare name without changing extension
  load order.

### Registration flow (in the loader-bridge wrapper)

When an extension registers a scalar/aggregate/vtab/hook function
through `spi-loader.register-*`, the host's loader-bridge wrapper:

1. Reads the extension's manifest for `(preferred-prefix, expansion)`.
   If absent, falls back to the deprecation-window synthetic
   expansion `sqlink-internal://<crate-name>` + warns. After
   v1.1 this becomes a hard error.
2. Looks up the prefix in `__sqlink_prefix`:
   - If exists with matching expansion → use it.
   - If exists with different expansion → fall back to numbered
     alternative (`foaf2`, `foaf3`, ...) per Q1 resolution + warn.
     Operator can `.prefix rename` after.
   - If absent → insert it.
3. Inserts the function into `__sqlink_prefix_function` keyed
   by `(expansion, function_name, n_args)`.
4. **Always** registers the function with SQLite under
   `prefix__function_name` (the qualified form). This is unconditional;
   qualified forms are always available regardless of bare-name state.
5. **Bare-name registration**: registers the function with SQLite
   under `function_name` too. This may shadow an earlier registration
   (per SQLite's last-wins semantics) — that's intentional, it
   preserves current behavior.
6. **Pin override**: if `__sqlink_prefix_pin` has a row for
   `(function_name, n_args)` pinning a different expansion, after
   the registration the wrapper re-registers the bare name pointing
   at the PINNED expansion's implementation (so the pin survives
   load-order changes).
7. **Collision logging**: if step 3 detected ANY existing
   `_prefix_function` row for the same `(function_name, n_args)`
   from a different expansion, emit a load-time warning naming all
   colliding extensions + which one currently owns the bare name +
   the available qualified forms.

The bare name dispatches per SQLite's normal function-table rules;
the wrapper does NOT intercept call-time dispatch. The only call-
time SQLite sees is whatever was last registered (or the pinned
form if a pin is set). This keeps existing SQL working
identically.

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

-- pin operator-controlled bare-name dispatch on collision
.prefix prefer concat exta         -- bare `concat()` dispatches to exta's
                                   -- implementation regardless of load
                                   -- order. Writes _prefix_pin row.
.prefix unprefer concat            -- removes the pin; bare-name reverts to
                                   -- SQLite-default last-registered-wins.

-- diagnostics
.prefix conflicts                  -- bare-name ambiguities currently in
                                   -- effect: function | n_args | bare owner
                                   -- | other callable qualified forms | pin
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
| `name(...)` (unique) | Works — dispatches to the one registered extension. |
| `name(...)` (collision, no pin) | Works — dispatches per SQLite default (last-registered wins). Load-time warning logged. Qualified forms available for explicit dispatch. |
| `name(...)` (collision, pinned via `.prefix prefer`) | Works — dispatches to the pinned extension regardless of load order. |
| `unknown__name(...)` | `no such function: unknown__name` — short prefix unknown. (SQLite-default error message.) |

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
| Loader-bridge wrapper: registration with disambiguation (scalar + aggregate) | 1 day |
| Loader-bridge wrapper: vtab + hook shapes (Q4 broader scope) | 1.5 days |
| Manifest field parsing + deprecation-window fallback | 0.5 day |
| `prefix-cli` extension scaffold + six dot-commands | 1 day |
| Function dispatch + bare-name shadowing on collision | 1 day |
| Native integration tests (round-trip + collision + rename, all 4 shapes) | 1.5 days |
| Browser smoke + docs | 0.5 day |

**Total: ~7.5 days for v1.** (Up from 5.5 days; Q4's "all four shapes
uniformly" adds vtab + hook coverage to the wrapper + tests.)

## Dependencies

- `spi.execute` against the user db (already exists).
- `spi-loader.register-scalar`/`register-aggregate`/`register-vtab`/
  `register-hook` — already exists; this plan wraps them.
- The loader-bridge's manifest-reading code (already exists).

No new substrate / capability variants required.

## Sequencing

1. Land **schema + migration** (`__sqlink_prefix` +
   `__sqlink_prefix_function`). Small standalone commit.
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

## Resolved design decisions

1. **Prefix-collision auto-fallback (Q1).** Auto-assign a numbered
   alternative + warn. When extension B claims `foaf` but the prefix
   is already bound to extension A's expansion, the loader binds
   extension B to `foaf2` (or the next free `foafN`), logs both
   expansions, and continues. Operator can `.prefix rename foaf2
   <better>` after load. Always allows progress; surfaces the
   collision in operator-visible warnings; works in non-interactive
   contexts (scripts, browser, daemons) where prompting is impossible.

2. **`last_used_at` update policy (Q2).** Updated only on
   operator-initiated CLI commands (`.prefix list / functions /
   expansion / verify`). Function-dispatch events do NOT write.
   Zero per-call overhead — important for tight query loops + WAL
   contention avoidance. Tradeoff: a prefix used heavily in queries
   but never explicitly inspected reads as "cold" to `.prefix gc`,
   but pruning is operator-driven so the operator can adjust the
   policy or `.prefix verify` periodically to refresh.

3. **Deprecation window (Q3).** Tied to the v1.1 release, NOT to
   calendar time. v1 ships the synthetic-expansion fallback +
   warning; v1.1 makes missing `preferred-prefix` /
   `prefix-expansion` a hard load-rejection error. Operators get a
   full release cycle of warnings to update out-of-tree extensions.
   In-tree extension audit (per the sequencing list below) ensures
   every workspace extension is migrated before the v1.1 cutover.

5. **Backwards compatibility for existing SQL (Q5, hard
   constraint).** Users today call `uuid_v4()`, `json_extract()`,
   etc. without any prefix. That must keep working exactly the
   same way after this feature lands, regardless of how many
   extensions are loaded or whether any of them collide on the
   bare name. The conflict-resolution policy was REVISED from
   the original "both-explicit" (which errored on bare-name
   collision) to "**bare name preserves SQLite's existing
   semantics; qualified forms are purely additive**":

   - When no collision: bare name + qualified form both registered.
   - When collision: bare name follows SQLite's last-registered-
     wins default (existing behavior); qualified forms ALWAYS
     available; load-time warning so the operator can see the
     collision; operator can `.prefix prefer` to pin the bare
     name to a specific extension.
   - The feature is **strictly additive** — it never breaks an
     existing SQL call, only adds new expressive capabilities
     (qualified dispatch, visibility into collisions, operator
     pin).

   New `__sqlink_prefix_pin` table introduced to back the
   operator-pin functionality.

4. **Function-shape coverage (Q4).** All four shapes — scalar,
   aggregate, vtab, hook — get prefix-namespaced uniformly in v1.
   Bigger scope than my recommended "scalar + aggregate only", but
   the user picked uniform treatment so the system is consistent
   across the SQL surface from day one. Two edge cases this introduces:
   - **Vtab USING syntax**: `CREATE VIRTUAL TABLE foo USING
     foaf__myvtab(...)` — the prefix appears in the USING module
     name. Operator picks the table name `foo` separately so no
     implicit collision at the table-name layer, but the USING
     module name shows the prefix.
   - **Hook namespace**: collations / commit-hook / window-functions
     each have their own SQLite dispatch surface. The wrapper needs
     per-shape implementations of the bare-vs-qualified logic. v1.1+
     can refine if specific hook shapes turn out to need different
     semantics.

## References

- SPARQL 1.1 Query Language, §4 "RDF Term Syntax" (prefixed names).
- SQLite identifier syntax (`https://sqlite.org/lang_keywords.html`).
- PLAN-bundles.md (sibling plan; same dispatch-bridge + manifest
  pattern this plan extends).
