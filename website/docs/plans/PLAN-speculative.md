# Plan: speculative items (consumer-driven)

> **Status: drafted, deliberately unstarted.** Every item here
> has the same problem  the design space is wide enough that
> picking a point without a real user steering risks shipping
> the wrong shape and locking in a bad contract. This doc
> captures what we'd do **if** a concrete consumer asked, so the
> design conversation can pick up at "you said X; we'd build Y"
> instead of "what should we build?"

Five items, ordered by how close each is to having an obvious
consumer.

| # | Item | Consumer signal we're waiting for |
|---|---|---|
| 1 | Writable CSV / excel / zipfile | A user writing tabular data from SQL who can't use INSERT INTO a normal table |
| 2 | Session Phase 2/3 (capture + apply WIT plumbing) | A replication / offline-sync / CDC consumer for SQLite |
| 3 | Per-engine wasm_memory64 + tighter wasmtime config | A workload pushing >4 GB of working set |
| 4 | Extension dependency declarations | Multi-extension catalogs where one ext requires another |
| 5 | Browser runtime | A browser-first deployment story |

Each section: **goal**  what we'd build; **what to ask the
consumer**  the open questions that need answers before
starting; **scope**  rough effort; **acceptance**  what shipping
looks like.

---

## 1 — Writable file-backed vtabs (CSV / excel / zipfile)

### Goal

Extend the existing read-only `csv`, `excel`, `zipfile`
extensions with INSERT/UPDATE/DELETE that write back to the
underlying file. The vtab-mutating contract (commit `b334c43`)
provides the infrastructure  xUpdate + xBegin/xCommit/xRollback
are wired across WIT and embed paths. The `csv` extension's
embed path (commit `d11ffb2`) is the proof port; excel and
zipfile are the same shape with a different file format.

### What to ask the consumer

The design hinges on three orthogonal questions; each has a
defensible default but the wrong default for a specific user
imposes weeks of rework:

1. **Persistence model.** Three options, very different
   characteristics:
   - **(a) Buffer until COMMIT, atomic temp+rename.** What
     `csv` ships today on the embed path. Strong correctness;
     transaction-aware; can OOM on big files.
   - **(b) Append-only INSERT, batch UPDATE/DELETE in memory.**
     Append-only is cheap (single fd_write at COMMIT); mutations
     buffer like (a). Hybrid pays only when you mutate.
     Doesn't work for excel (workbook is structural, not
     appendable).
   - **(c) Mutate-in-place with periodic background flush.**
     What sqlite itself does. Most complex; requires careful
     fsync semantics; only worth it if the user has very large
     files they want to write to incrementally.

   **Ask:** "What's the largest file you'll mutate in a
   transaction? Do you need partial failures to leave the
   file consistent, or is 'commit succeeded or commit didn't
   happen' the contract?"

2. **Concurrency.** Three options:
   - **(a) No locking, single-writer assumed.** Matches our
     wasm sandbox today; same as what `wasivfs` does.
   - **(b) File-lock on open** (flock / fcntl). Multi-process
     coordination; defeats wasi's lack of locking primitives.
     Don't ship without a real ask.
   - **(c) Optimistic concurrency** with version tag in a
     side channel. Heavier; only for shared-file scenarios.

   **Ask:** "Is this file only written by one cli at a time,
   or do you have multiple readers + writers?"

3. **Schema evolution.** Two options:
   - **(a) Fixed schema** declared at CREATE VIRTUAL TABLE
     time. What csv embed does today (8 generic TEXT columns).
     INSERT with fewer columns pads NULLs.
   - **(b) Schema follows the file** (header row for csv,
     sheet structure for excel). Auto-grow on first INSERT;
     migrate on schema change. More work; only useful if the
     user wants to use the vtab as the source-of-truth.

   **Ask:** "Will the user's SQL hardcode column names, or do
   they want the vtab to discover the schema from the file?"

### Scope

Per extension, assuming the consumer's answers are
(a)/(a)/(a) (defaults shipped on csv):
- `excel`: ~half day. xlsx is a zip of XML  serialize
  changes via `umya-spreadsheet` (already a dep) into the same
  workbook structure on commit.
- `zipfile`: ~1 day. ZIP entries are positional; append at
  commit. DELETE means rewriting the whole archive  same
  shape as csv's temp+rename.

If the consumer's answers diverge from defaults, each (b)/(b)
or (c)/(c) change adds 1-3 days for design + implementation +
the test surface that goes with concurrency / schema-evolution.

### Acceptance

1. The matching `embed.rs` ships with `update / begin / commit /
   rollback` Some(fn) entries
2. INSERT / UPDATE / DELETE round-trip through the file (open
   fresh cli, query, see the writes)
3. BEGIN + INSERT + ROLLBACK leaves the file unchanged
4. PRAGMA integrity_check returns ok if we wired xIntegrity
5. Smoke covers the 3-op cycle + the rollback path

---

## 2 — Session Phase 2/3: capture + apply via WIT host plumbing

### Goal

Let WIT-path extensions capture changesets from the cli's
sqlite connection and apply them to either the local
connection or a different one. Phase 1 (`05b69ce`) shipped
the pure-function half  invert, concat, decode  as
SQL scalars. Phase 2 = apply; Phase 3 = full capture with
session lifecycle.

### What to ask the consumer

Three open questions:

1. **Who owns the session handle?** Options:
   - **(a) Implicit, per-connection.** Open the cli with
     `--session=watch:<table>`; every commit fires a hook the
     extension can read. No SQL-level handle.
   - **(b) SQL-level handle** via `session_create(name, db, table)`
     returning an opaque integer the extension threads through
     subsequent calls. Easier to model; harder to clean up if
     the cli crashes mid-session.
   - **(c) Vtab-as-handle.** `CREATE VIRTUAL TABLE s USING
     session('main', 'mytable')`; sessions live as long as the
     vtab. Idiomatic; heaviest to implement.

   **Ask:** "Is the changeset stream tied to the cli's lifetime
   or to a SQL-driven scope (transaction / explicit start/end)?"

2. **Where do changesets land?** Options:
   - **(a) Sink to a file.** `changeset_apply(blob)` reads
     bytes; capture writes bytes. Symmetric, file-based.
     Matches the existing `.session` dot-command + the
     `sqlite-wasm-run changeset` subcommand.
   - **(b) Sink to a SPI buffer.** Extension exposes a
     callback the host invokes with each captured change.
     Streamable; usable for replication consumers that want
     row-level granularity.
   - **(c) Sink to another db.** Capture from one db, apply to
     another atomically  the apply path is host-side, not
     guest-side.

   **Ask:** "Do you want bytes you can ship somewhere, or
   do you want to react to changes inside SQL?"

3. **Transactional semantics on apply.** Two options:
   - **(a) Apply-as-one-transaction.** Either all changes
     succeed or none. Easy; what sqlite's own
     `sqlite3changeset_apply` does by default.
   - **(b) Apply-with-conflict-resolution.** Per-row callbacks
     for OK / OMIT / REPLACE / ABORT. Heavier; needed for
     real merge / CDC consumers.

   **Ask:** "If a row in the changeset doesn't match the
   target db's current state, do you want to retry? Skip?
   Bail?"

### Scope

- Phase 2 (apply only) with default answers (a/a/a): ~3-5 days.
  New WIT `session-spi.apply(blob)` import; host wraps
  `sqlite3changeset_apply`; `extensions/session-apply` SQL
  surface.
- Phase 3 (full capture) with (b/b/b): ~5-7 days. Session
  handle lifecycle is the dragon; the WIT side is straightforward.

Both add to the host's surface (new dispatch methods) and the
cli's compile flags (`SESSION + PREUPDATE_HOOK` already enabled
in `LIBSQLITE3_FLAGS`).

### Acceptance

- New `session-spi` WIT interface
- Host dispatch methods + HostWrap impls
- One reference extension exercising the surface
- End-to-end smoke: capture from db A  apply to db B  same
  query returns same rows

---

## 3 — Per-engine wasm_memory64 + tighter wasmtime config

### Goal

Move beyond the perf-push wins by either (a) supporting
wasm64 guests so a single linear memory can address >4 GB, or
(b) adding wasmtime config knobs that pay off only for very
specific workload shapes.

### What to ask the consumer

The signal here is concrete:

1. **What's your working-set size?** Three regimes:
   - **<256 MB:** Stay on wasm32; current config is already
     near-optimal. Nothing to do here.
   - **256 MB - 4 GB:** Stay on wasm32; increase
     `memory_reservation` and add a `--max-memory` flag for
     the cli. ~1 day.
   - **>4 GB:** Switch to wasm64 guests. Multi-week project
     touching libsqlite3-sys (no wasm64 prebuilt), cli build
     flags, host config, and probably wasi-libc itself.
     Defer unless this is the actual blocker.

   **Ask:** "How much linear memory does your workload need
   to keep resident at once?"

2. **What are you tuning for?** Options:
   - **(a) Throughput** (more wall-clock work per second).
     Look at `cranelift_pcc(true)` (proof-carrying code
     elimination), bigger `compilation_cache`, more `pooling`.
   - **(b) Latency** (predictable per-call time). Look at
     `disable_async`, single-stack instantiation, no fuel
     (already done for cli).
   - **(c) Memory footprint** (small instances). Look at
     `static_memory_maximum_size` reduction, less
     `pooling`, smaller stacks.

   **Ask:** "What's the metric you're trying to move? p99
   latency? steady-state throughput? RSS?"

3. **Are you OK with non-portable artifacts?** The current
   `.cwasm` precompile already binds to a wasmtime version
   + cpu arch. More aggressive lowering (e.g.
   `cranelift_use_egraphs`, custom relocations) makes it
   more brittle.

   **Ask:** "How often do you regenerate the .cwasm? Once
   per machine? On every deploy?"

### Scope

- Defaults investigation: ~half day each. The work is
  largely measurement, not code.
- wasm64 support: 2-4 weeks. Out of scope without a hard ask.

### Acceptance

- Whatever metric the consumer named moves in the right
  direction by a measurable amount
- No regression on the existing bench matrix

---

## 4 — Extension dependency declarations

### Goal

Let an extension declare "I require `vec` to be loaded
first" via a `requires-spec` field in the manifest. The
cli's `.load` flow checks declared dependencies against the
loaded set, fails fast if missing, and can later auto-
resolve via the registered resolvers.

### What to ask the consumer

1. **What level of dependency are we resolving?** Three
   options:
   - **(a) Function-level**: "I call `vec_f32`; whoever
     provides it is fine." Loose binding; works with any
     registered vec implementation.
   - **(b) Extension-name + version**: "I require `vec >=
     0.2`." Stronger; matches the manifest contract.
   - **(c) Cryptographic pin** by blake3 digest. Reproducible
     builds; matches the existing trust-policy infrastructure
     (commit `f7…`).

   **Ask:** "Do you want soft dependencies (any matching
   function), versioned dependencies (semver), or pinned
   (exact bytes)?"

2. **Auto-resolve or fail-fast?**
   - **(a) Fail-fast**: `.load A` errors if A requires B and
     B isn't loaded. User must `.load B` first.
   - **(b) Auto-resolve**: `.load A` looks up B in the
     resolver registry and loads it transitively.

   **Ask:** "Should the cli load dependencies for the user
   or refuse to load until they've already done so?"

3. **What does a circular dep error look like?** Required
   if (b); not relevant if (a).

### Scope

- Minimum viable (a) + fail-fast: ~1-2 days. WIT field,
  host check, error message.
- Full (c) + auto-resolve: ~3-5 days. Trust verification per
  loaded extension on the resolution path.

### Acceptance

- `requires-spec` field in `metadata::Manifest`
- Cli `.load` checks; clear error on missing
- Smoke: extension A requiring B fails without B; succeeds
  with B loaded; auto-resolves if the resolver is wired

---

## 5 — Browser runtime

### Goal

Run the cli in the browser via `jco`-style wasi-shim
adapters, so the same `.component.wasm` works in a webpage as
in `sqlite-wasm-run`.

### Why this is separately filed

A separate plan (`PLAN-browser-runtime.md`) already exists
for this. The work is its own project  separate from the
catalog and the perf push. Not duplicated here.

### What to ask the consumer

1. Is this a "the cli runs in a browser" play, or "extensions
   load in a browser" play?
2. Persistent storage  IndexedDB? OPFS? In-memory?
3. Performance target  comparable to native wasmtime, or
   "works at all"?

### Acceptance

Whatever `PLAN-browser-runtime.md` defines.

---

## The anti-pattern this doc is structured to prevent

The risk with speculative work is **drift**: we ship one
defensible point in a wide design space, the consumer's actual
needs land somewhere else in that space, and we rework the
contract because the bits we picked were wrong for the user.

The mitigation is the "what to ask the consumer" section in
each item. When someone asks for one of these:

1. Pull this doc up.
2. Read the consumer-questions for the relevant item.
3. Ask them. Don't guess.
4. Write the answers as a "**Consumer:** …" preamble to the
   section, then start implementing against that pin.
5. The body of the section then becomes the
   to-be-translated-to-tasks plan.

This is the same discipline `PLAN-tooling-and-session.md`
applied to session phase 2/3, and the reason phase 1 shipped
clean while phase 2/3 are still appropriately deferred.

---

## Cross-cut: what shipping ANY of these requires

Independent of which item, every entry on this list shares
infrastructure that's already in place:

- WIT contract evolution path (`sqlite-loader-wit/wit/guest.wit`,
  the dispatch.wit pair, regen the host bindings, bump every
  extension's manifest if a manifest field changes)
- Host dispatch + HostWrap impl pattern
- Cli trampoline pattern for vtab work
- Embed-helper extension pattern in `sqlite-embed`
- Test bed via `extensions/inmem` (writable vtab) or a new
  similar minimal extension

So when one of these unlocks, the prerequisite plumbing is
known-good; the work is in the per-item design, not the
infrastructure.
