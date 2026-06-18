# Plan: T-39, T-40, and session/changeset

Three deferred items, ordered smallest-to-largest. Each section
has the same shape: goal, approach, cost, tradeoffs, open
questions. Pick one or three; they're independent.

---

## T-40: smoke harness --db override

### Goal

Let smoke tests for spi-dependent extensions (eval, db-utils,
template, completion-with-schema) run against a real file-backed
database instead of `:memory:`. Currently every spi.execute()
call errors with "spi requires a file-backed database" because
the host sqlite3 and the wasm-internal sqlite3 are separate
libraries with separate page caches.

### Approach

Per-extension marker in smoke.sql first line. Parse a directive
like:

```sql
-- smoke-db: tempfile
.load extensions/eval/...
SELECT eval('SELECT 1');
```

Implementation in `tooling/smoke.py`:

1. Read smoke.sql's first 5 lines, search for `-- smoke-db:` prefix.
2. If found and value is `tempfile`: create a fresh `tempfile.NamedTemporaryFile(suffix=".db")` per smoke run.
3. If value is a path: use that path (advanced; for shared dbs).
4. Default (no marker): `:memory:` as today.
5. Pass `--db <chosen>` to sqlite-wasm-run.
6. Clean up tempfile in a `finally` block.

Also need to re-seed eval/smoke.expected once it can actually run.

### Cost

- ~40 LOC change in tooling/smoke.py
- ~5 changed smoke.sql files (eval, db-utils ship one for the
  first time, optionally template / completion later)
- 1 commit

### Tradeoffs

- **Marker-in-comment vs separate config file**: comment-in-smoke
  keeps the config visible to anyone reading the smoke; separate
  file would mean an extra path to remember. Marker wins.
- **Per-run tempfile vs shared persistent db**: tempfile is
  always clean. Shared would be needed if a smoke wanted to test
  cross-statement persistence within a single run; can add later
  as `-- smoke-db: persistent:foo.db` if a real consumer asks.
- **What about T-19's auto-injected `.nullvalue`?**: orthogonal.
  Both injections coexist; tempfile is just an argv tweak.

### Open questions

- Should `make ext-ship` parallel mode (T-17) also use per-extension
  tempdirs? Currently it uses a tempdir for the component cache
  but a shared `:memory:` db. If we add per-extension tempdb,
  parallel mode needs to allocate one tempdb per smoke too.
  Probably fine — same model as the cache dirs.

---

## T-39: scaffold --world flag

### Goal

`python3 tooling/scaffold.py myext --world tabular` should
generate a fresh extension skeleton with the RIGHT world wiring
(WIT import, Guest impls, manifest fields) instead of the
default `minimal`. Recent ships that needed hand-edits: `uint`
(collating), `completion` (tabular). Future ports of session
or other vtab/collating/stateful work will hit this too.

### Approach

Replace the single `tooling/templates/lib.rs.tmpl` with
per-world templates:

```
tooling/templates/
  lib.rs.minimal.tmpl       (existing  unchanged)
  lib.rs.collating.tmpl     (collation Guest + empty scalar)
  lib.rs.tabular.tmpl       (vtab Guest + cursor state + thread_local)
  lib.rs.stateful.tmpl      (aggregate Guest + per-instance state)
  lib.rs.authorizing.tmpl   (authorizer Guest)
```

The `tabular.tmpl` is by far the biggest (~250 LOC) because vtab
boilerplate includes best_index, open/close, filter, next, eof,
column, rowid, create/connect/destroy/disconnect. Listargs is
the right starting point; strip the listargs-specific parts and
leave the scaffold.

scaffold.py changes:

1. `--world {minimal,collating,tabular,stateful,authorizing}`
   default `minimal`.
2. Validate the world name against the WIT (or just hardcode the
   list since worlds rarely change).
3. Load `lib.rs.<world>.tmpl`, render with NAME / CRATE_DEPS.
4. Print "next steps" hints specific to the world (e.g. for
   `tabular`: "edit the cursor state struct + best_index logic").

### Cost

- ~30 LOC scaffold.py change
- 4 new template files, each ~150-300 LOC of scaffolded code
  - collating: ~120 LOC (just CollationGuest + empty scalar)
  - tabular: ~250 LOC (full vtab skeleton)
  - stateful: ~180 LOC (aggregate Guest + RefCell state map)
  - authorizing: ~100 LOC (AuthorizerGuest only)
- 1 commit

### Tradeoffs

- **One unified template with conditional sections vs N
  per-world templates**: unified would use python's `.format()`
  with conditional sections, which is awkward. N templates are
  duplicative but each is independently readable. Per-world wins.
- **Should scaffolded extensions PASS smoke immediately?**:
  ideally yes  the "fresh scaffold compiles and PASSes" behavior
  is one of the scaffold's documented features (T-16). Tabular
  is the tricky one  it'd need a no-op vtab that returns zero
  rows. Doable; ~5 extra LOC of stub.
- **What if WIT gains a new world?**: someone adds a template
  file. Same workflow as today (`tooling/snippets/README.md`
  pattern  documented place, additions cheap).

### Open questions

- Should `--world stateful` ALSO auto-include the spi import
  hint? Most stateful extensions use spi. Probably yes; a single
  commented-out `use bindings::sqlite::extension::spi;` line in
  the template is enough of a nudge.
- Should the template's smoke.sql also vary by world? `tabular`
  smokes want `SELECT * FROM ext_name` not `SELECT scalar()`.
  Light per-world variation in smoke.sql.tmpl too.

---

## Session/Changeset API

### Goal

Port SQLite's `sqlite3session_*` and `sqlite3changeset_*` C APIs
so extensions can capture, transform, and apply database
changesets in SQL. This is genuinely deferred per
PLAN-sqlite-plugins.md (and the plan-doc's "free via flag" was
optimistic  the flag isn't even set today).

### What's actually in the session API

Two halves:

**Capture half (needs a live connection):**
- `sqlite3session_create(db, dbname, &session)`
- `sqlite3session_attach(session, table_name)` / detach
- `sqlite3session_changeset(session, &nBlob, &pBlob)`  serialize
- `sqlite3session_patchset(...)`  smaller subset (no before-vals)
- `sqlite3session_delete(session)`

**Transform / apply half (some need connection, some are pure):**
- `sqlite3changeset_apply(db, blob, ...)`  applies to a db
- `sqlite3changeset_invert(blob)  blob`  pure function
- `sqlite3changeset_concat(a, b)  blob`  pure function
- `sqlite3changeset_start(...)` + walk  iterator (pure)

The PURE functions transform changeset blobs without touching a
database. The CAPTURE and APPLY functions need the host's
connection (the wasm-internal sqlite3 isn't where the user data
lives  same constraint as eval/T-40).

### Approach: three phases by increasing cost

**Phase 1: pure-function changeset helpers (1-2 days)**

Ship a scalar extension `changeset` with the connection-free
operations. The changeset blob format is documented and small;
re-implementing the parser in Rust is ~400 LOC.

Scalars:
- `changeset_invert(blob)  blob`
- `changeset_concat(a, b)  blob`
- `changeset_decode(blob)  json`  human-readable for debug
- `changeset_count(blob)  integer`  number of operations
- `changeset_tables(blob)  json`  list of affected tables

Smoke uses a hardcoded reference changeset blob (capture once
from real sqlite3, lock the bytes) so we can verify without
needing the capture half.

Pros: no host changes, no architecture work, ships independently.
Cons: doesn't include capture or apply  the headline operations.

**Phase 2: apply-only host extension (3-5 days)**

Add SPI plumbing so a wasm extension can ask the host to apply
a changeset to its connection. New WIT import:

```wit
interface session-spi {
    use types.{sqlite-error};
    changeset-apply: func(blob: list<u8>) -> result<s64, sqlite-error>;
    changeset-apply-strm: func(blob: list<u8>, ...) -> ...;
}
```

Plus the host-side impl wrapping libsqlite3-sys's session FFI.
Note `SQLITE_ENABLE_SESSION` + `SQLITE_ENABLE_PREUPDATE_HOOK`
flags need adding to Makefile's SQLITE_CFLAGS.

Extension `changeset_apply(blob)` becomes the SQL surface.
Inherits the T-40 file-backed-db smoke constraint.

Pros: covers the "import changes into this DB" use case (offline
sync, replication consumers).
Cons: still no capture; user can't generate changesets, only
consume them.

**Phase 3: full capture (5-7 days)**

Same shape as phase 2 but for the capture half: session creation,
attach, changeset extraction. The complexity is the session
HANDLE lifecycle  sessions are stateful (think of them like
prepared statements) and the wasm extension needs to hold a
handle across multiple SQL calls.

Options for handle lifetime:
- Numeric handle returned to SQL, threaded through subsequent
  calls. Risk: callers leak sessions, host accumulates state.
- Single "default session" per connection. Simpler, less
  flexible. Probably right for v1.
- Session-as-vtab: `CREATE VIRTUAL TABLE s USING session('main')`
  encapsulates the lifetime in vtab create/destroy. Most idiomatic
  but heaviest to implement.

WIT additions: session interface with create/attach/extract/delete
host functions, extension wraps in SQL surface.

Pros: complete API parity with sqlite3session.
Cons: handle lifecycle is the kind of state-bridging problem
that takes time to get right; phase 1+2 may cover 80% of real
use cases.

### Cost summary

| Phase | Scope | Days | Risk |
|---|---|---|---|
| 1 | pure helpers | 1-2 | low (no host changes) |
| 2 | apply-only | 3-5 | medium (new WIT, host plumbing) |
| 3 | full capture | 5-7 | high (handle lifecycle, state) |

Phase 1 is the "ship soon" item. Phase 2+3 are real architectural
work that benefits from a concrete consumer (someone actually
asking for replication / sync) to validate the design before
investing.

### Tradeoffs

- **Re-implement changeset format vs link against libsqlite3-sys
  session functions**: re-implementing in pure Rust means the
  wasm extension doesn't need the host's session API. Linking
  ties the wasm to host plumbing. For phase 1, re-implementing
  is cleaner.
- **One extension `session` vs two extensions `session-capture`
  + `session-apply`**: split makes phasing cleaner (ship apply
  before capture). Merge later if the surface grows.
- **What about patchsets?**: patchsets are a subset of
  changesets (no before-values). The format parsers can share
  ~80% of code. Phase 1 should support both.

### Open questions

- Does anyone actually need this? The deferral has held for the
  entire catalog build-out. Phase 1 ships value with no host
  change so it's cheap insurance; phase 2/3 should wait for a
  concrete request.
- Should we adopt sqlite-vss-style "use the host directly via
  FFI bindings" pattern (vec0 does this for its k-NN backends)?
  Different architecture; would let extensions call host APIs
  without WIT plumbing for narrow integrations.

---

## Recommended sequencing

1. **T-40 first** (smallest, unblocks eval/db-utils/template/
   completion-phase-2 smokes).
2. **T-39 second** (independent; removes friction from every
   non-minimal ship; pays back compounding).
3. **Session phase 1** (the pure-function helpers; ships value
   with no host changes; cheap).
4. **Session phase 2/3** ONLY if a concrete consumer materializes.
   Until then, phase 1's helpers + the documented limitation
   are the right shape.

Total committed work for steps 1-3: ~2-3 days.
