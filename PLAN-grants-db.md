# Plan: persist component capability grants + orchestration definitions in the database

> **Status (2026-06-15): all phases shipped.**
> - **G1**: `_capability_grants` table + `cli/src/grants.rs`
>   helpers (`ensure_schema`, `put`, `get`, `list`, `delete`)
>   live in-tree; `do_load` consults the table to seed Policy.
> - **G2**: `.grants` dot-command family (`list`, `get`,
>   `revoke`, `delete`) shipped via `cli/src/lib.rs::do_grants`.
> - **G3**: digest pinning lives in the table schema; `.load
>   --trust=manifest|stored` (`TrustMode` enum) covers fast-path
>   trust + pinned-digest verification.
> - **G4**: `cli/src/orchestration.rs` ships the in-tree
>   `_compose_plans` storage against `sqlite_wasm_core::db`; the
>   parallel orchestrator-side crate
>   `~/git/webassembly-component-orchestration/libs/compose-store-sqlite/`
>   reads/writes the same schema (cross-tool interop test in
>   that crate's `tests/cli_interop.rs`).

## Goal

Two related-but-separable capabilities:

1. **Capability grants in the database**. Today every `.load`
   applies a fresh `Policy` derived from the extension's
   manifest's `declared_capabilities`, gated by the host's
   `TrustPolicy`. The grant decision is **ephemeral** —
   nothing on disk records which extensions a user has
   approved with which capabilities, so trust is rebuilt from
   scratch on every cli session. Persist grants so they
   survive sessions, audit cleanly, and support a
   trust-on-first-use UX.

2. **Orchestration definitions in the database**. The
   webassembly-component-orchestrator project (sibling repo,
   currently not checked out on this machine) emits component
   composition recipes — a definition language for "compose
   bridge X against components A, B, C, satisfy imports D".
   Today those definitions live as files. Storing them in the
   user's database makes them addressable by name from SQL
   and survives across hosts. Per the user's note, the storage
   layer is better lifted into a dedicated crate inside the
   orchestrator project that sqlite-wasm imports.

## Current state

### Capability surface (relevant types)

`sqlite-loader-wit/src/lib.rs`:
- `Capability` enum — 11 variants (Spi, Prepared, Transaction,
  Schema, State, Cache, Random, Text, Hashing, Encoding, Http).
- `HttpPolicy` — allowlist of hosts, methods, body cap,
  timeout.
- `Policy` — granted set + http + fuel + memory + epoch
  deadline.

`host/src/lib.rs`:
- `TrustPolicy` (line 1579) — AllowAll / DigestAllowlist /
  DenyAll / Ed25519Signed.
- `LoadedExtension.policy` field holds the per-extension
  `Policy` after a successful load.
- No persistence anywhere.

### Load path (where grants get applied)

`.load` → host's `register_*` / `load_*` →
manifest.declared_capabilities → check against TrustPolicy →
construct `Policy` → stash on `LoadedExtension`. Lives until
the extension is unloaded or the cli exits.

## Architectural questions to settle

### Q1. Where does the grants table live?

Three locations possible:

- **(A) User's connected database** as `_capability_grants`
  table. Pro: discoverable from SQL (`SELECT * FROM
  _capability_grants`), backups travel with the data, no
  out-of-band path config. Con: pollutes the user's schema;
  one grant set per database (the same extension loaded
  against `db1.sqlite` vs `db2.sqlite` has independent
  grants).
- **(B) Sidecar SQLite file** at `~/.sqlite-wasm/grants.db`
  (or `XDG_CONFIG_HOME`). Pro: single source of truth across
  every database the user opens; matches how ssh/gpg/etc.
  separate trust from data. Con: out-of-band config, doesn't
  travel with the data file, multi-user collisions.
- **(C) `sqlite_*`-prefixed metadata table** inside the user
  db. Pro: SQLite tooling already filters these out from
  schema dumps. Con: SQLite reserves the prefix; safety is
  fragile and undocumented.

**Decision**: (A) `_capability_grants` in the user database.
The leading underscore signals "infrastructure, do not
edit" the same way `_litestream_seq` and similar
infrastructure tables do. Per-database grant scoping matches
how `.load` behaves today (extensions register against the
open connection, not globally). A sidecar pattern can be
added later as a second-tier override if multi-db trust
federation becomes a real need.

### Q2. Schema shape — single JSON column or normalized?

```sql
-- Option (a) — denormalized JSON
CREATE TABLE _capability_grants (
    extension_name TEXT PRIMARY KEY,
    digest_hex     TEXT,          -- blake3 of provider bytes
    policy_json    TEXT NOT NULL, -- {granted: [...], http: {...}, fuel: N, ...}
    granted_at     TEXT NOT NULL, -- ISO-8601
    granted_by     TEXT,          -- "user" / "default" / "cli-arg"
    notes          TEXT
);

-- Option (b) — normalized
CREATE TABLE _capability_grants (
    extension_name TEXT PRIMARY KEY,
    digest_hex     TEXT,
    granted_at     TEXT,
    granted_by     TEXT,
    fuel_per_call  INTEGER,
    memory_limit   INTEGER,
    epoch_ms       INTEGER
);
CREATE TABLE _capability_grant_caps (
    extension_name TEXT,
    capability     TEXT,
    PRIMARY KEY (extension_name, capability)
);
CREATE TABLE _capability_grant_http_hosts (...);
```

**Decision**: (a) JSON column. The capability set is small
and changes through one code path (the host) that already
serializes/deserializes the entire `Policy`. Normalization
gives you queryable joins (`SELECT * WHERE capability = 'Http'`)
but at the cost of multi-table updates inside a single
.grant/.revoke. Convenience views over the JSON give us most
of the query benefit:

```sql
CREATE VIEW _capability_grants_pretty AS
SELECT extension_name,
       json_extract(policy_json, '$.granted') AS capabilities,
       json_extract(policy_json, '$.http.allowed_hosts') AS http_hosts,
       digest_hex, granted_at
FROM _capability_grants;
```

### Q3. When does `.load` consult the table?

```
.load <path>
  → host parses manifest
  → for each declared_capability in the manifest:
      lookup _capability_grants[extension_name]
      ├─ if row exists AND digest matches: apply stored Policy
      ├─ if row exists AND digest mismatch: ERROR — bytes changed
      └─ if no row: TOFU prompt OR auto-grant per CLI flag
```

**Q3a. TOFU prompt or auto-grant?**

Three modes:
- `--trust=prompt` (default in interactive cli): show
  capabilities, ask "approve? [y/N/details]". Persist on yes.
- `--trust=manifest`: auto-grant everything the manifest
  declares. Persist. Matches today's behavior; lets
  non-interactive scripts work unchanged.
- `--trust=stored`: only allow extensions with an existing
  stored grant. Fail otherwise. Hardened production mode.

**Decision**: ship all three, with `--trust=manifest` as the
default to preserve current behavior. `prompt` becomes the
better default once a TTY interactive UI exists; defer
flipping the default to that work.

**Q3b. Digest mismatch handling?**

Loading bytes whose blake3 differs from what was stored is
TOFU's whole point. Options:
- Hard error, force the user to `.revoke` and re-grant.
- Warn and re-prompt.
- Silently re-apply (dangerous — defeats integrity).

**Decision**: hard error by default; `--trust=manifest`
implies "accept new digest, update record".

### Q4. Identity — name vs digest?

Spoofing risk: an attacker who can write a file at the same
path as a trusted extension gets the trusted extension's
grants if the table is keyed by name only.

**Decision**: PRIMARY KEY is `extension_name` (the user-facing
handle), but `digest_hex` is recorded and checked on every
load. Name-based key keeps `.grant`/`.revoke` UX intuitive;
digest verification closes the spoofing window.

### Q5. Schema migration / version pinning?

Tables get auto-created at first relevant operation. The
SQLite cli already creates a few helper tables (the CAS cache
is one). Mirror that pattern: a `_capability_grants_meta(key,
value)` row carrying the schema version, checked on table
access, with a fixed migration function from each prior
version. v1 ships; v2+ is future work.

### Q6. Orchestration definitions — same plan or split?

Different shape:
- Capability grants are small (KB), checked on every `.load`,
  and naturally per-database scoped.
- Orchestration definitions can be large (a compose recipe
  references many components), are referenced by tooling
  outside sqlite-wasm (the orchestrator's own cli), and want
  to be addressable across many databases.

**Decision**: orchestration definitions get their own
storage path in a NEW crate inside
`~/git/webassembly-component-orchestrator` (currently not
checked out — flagged in this plan as a soft dependency).
The sqlite-wasm side imports that crate and exposes a thin
SQL surface: `.compose-from-db <name>` style. The storage
layer (table schema, migrations, query helpers) lives with
the orchestrator. This plan defines the integration contract
but doesn't implement the orchestrator-side crate.

## Phases

### Phase G1 — grants table + load-time application (~1 day)

- New helper module `host/src/grants.rs` with:
  - `ensure_schema(conn)` — auto-creates table + meta row
    on first use. Idempotent.
  - `get(conn, ext_name) -> Option<StoredGrant>` where
    `StoredGrant { policy, digest, granted_at, ... }`.
  - `put(conn, ext_name, grant)`.
  - `delete(conn, ext_name) -> bool`.
- Host's `register_*`/`load_*` consults grants before
  applying manifest policy:
  - Stored + matching digest → use stored.
  - Stored + mismatching digest → error.
  - No row → behavior controlled by `--trust=` flag.
- New cli flag plumbing: `--trust prompt|manifest|stored`
  (default `manifest`).

### Phase G2 — grant management dot-commands (~half day)

- `.grants` — list all stored grants for the current
  database. Output format mirrors `.tables`.
- `.grants show <ext>` — pretty-print the JSON policy for one
  extension.
- `.grants revoke <ext>` — delete the row. Next `.load` will
  re-prompt or fail per the `--trust` flag.
- `.grants approve <ext> [--caps=...] [--hosts=...]` —
  pre-grant before loading. Useful for non-interactive
  setup scripts.
- `.grants audit` — show recent grant additions (relies on
  `granted_at` ordering).

### Phase G3 — digest pinning + identity (~half day)

- Compute blake3 of provider bytes at load time.
- Store + check digest as part of the load decision matrix
  defined in Q4.
- On digest mismatch, print a clear remediation message:
  ```
  Error: extension 'postgis' was granted with digest
         <stored>, but the current bytes have digest <new>.
  To re-grant: .grants revoke postgis
              .load <path>
  ```

### Phase G4 — orchestration definition hook (~half day on sqlite-wasm side)

This is the integration contract; the storage layer itself
lives in the orchestrator project.

- Define a trait in sqlite-wasm-host:
  ```rust
  pub trait OrchestrationStore {
      fn get(&self, conn: &Connection, name: &str)
        -> Result<Option<OrchestrationDef>>;
      fn put(&self, conn: &Connection, name: &str,
        def: OrchestrationDef) -> Result<()>;
      fn list(&self, conn: &Connection)
        -> Result<Vec<String>>;
  }
  ```
- Default impl: `NullOrchestrationStore` (returns
  `Ok(None)` / errors with "orchestration not configured").
- Host exposes a registration hook
  (`Host::set_orchestration_store(impl OrchestrationStore)`).
- Add CLI dot-commands:
  - `.compose-list` — list stored compositions.
  - `.compose-from-db <name>` — load a stored composition.
  - `.compose-save <name> <path>` — persist a composition
    file by name.
- The store's table schema, JSON shape, and migration
  semantics are owned by the orchestrator project.
  sqlite-wasm imports the crate; the crate provides the
  impl. The integration shape is the contract, not the
  implementation.

## Total estimated effort

- G1: ~1 day
- G2: ~half day
- G3: ~half day
- G4: ~half day on sqlite-wasm side; the orchestrator
  crate it depends on is separately scoped. The plan ships
  with `NullOrchestrationStore` so sqlite-wasm builds
  cleanly even before the orchestrator crate exists.

**~3 days end-to-end** for the sqlite-wasm side. Orchestrator
crate work is a separate budget.

## Out of scope

- Multi-database grant federation. If a user runs the cli
  against `db1.sqlite` then `db2.sqlite`, each has its own
  grants. A sidecar `~/.sqlite-wasm/grants.db` overlay can
  be added later if a real workflow demands it.
- Time-bound grants ("auto-revoke after N days"). Easy to
  add to the JSON policy later; not v1.
- Grant inheritance / roles ("dev-trust" group). Adds
  complexity that the underlying SQL doesn't need yet.
- Encrypted policy_json columns. The data is plaintext
  capability declarations, not credentials.
- Network-syncing grant decisions (e.g. via a fleet manager).
  Out of architectural scope for cli-side trust.

## Implementation notes

- The grants table is per-database. Loading the same
  extension against two databases with two different grant
  sets is intentional, not a bug.
- TrustPolicy (DigestAllowlist / Ed25519Signed / etc.)
  applies BEFORE the grants table is consulted. Allowlist
  rejection means the load never reaches `_capability_grants`
  at all. This is intentional: trust-policy is the
  "is this binary allowed to run at all" gate; grants are
  "given that it can run, what can it do".
- The `_capability_grants` row is the source of truth at
  runtime — the host re-reads it on every `.load`, not just
  the first. Out-of-band edits via SQL (`UPDATE
  _capability_grants SET policy_json = ...`) take effect on
  the next load. That's a feature: power users can edit the
  table directly; standard users use `.grants`.

## Dependencies

- **Soft dependency on webassembly-component-orchestrator**
  for Phase G4. Currently not checked out at
  `~/git/webassembly-component-orchestrator`; the
  `NullOrchestrationStore` default lets G1-G3 ship without
  it. G4 SHIPS the integration hook but doesn't ship a real
  impl until the orchestrator crate exists.
- blake3 crate (host) — already a transitive dep through
  signature verification.
