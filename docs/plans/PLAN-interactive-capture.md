# Plan: interactive changeset capture (daemon vs WIT-extension)

> **Status (2026-06-22): deferred, awaiting consumer.** This is
> a design-rationale doc; the recommendation at the bottom is
> "ship neither yet" until a concrete external-tool caller
> materializes. Kept so the conversation can pick up where it
> left off when a consumer asks. Tracked alongside item 2 of
> `PLAN-speculative.md` (Session Phase 2/3).

Phase 3 of the session port (capture + apply) landed in commit
910afc2 via subcommands on `sqlite-wasm-run`. Trade-off:
**no interactive capture**  the user has to provide changes as
a SQL script.

For interactive capture, two architectures are reasonable. They
solve the same problem with different shapes; the answer
depends on who the caller is and what they want to look like.

---

## Option D: Daemon model

### How it would work

```
sqlite-wasm-run changeset daemon \
    --db PATH \
    --socket /tmp/sess.sock \
    --output captured.cs \
    [--table NAME]
```

- Opens the db, creates a session, attaches table(s).
- Listens on a unix-domain socket (or named pipe on Windows) for
  command messages.
- Other tools connect, send SQL through the socket; the daemon
  executes via `sqlite3_exec` on the SAME connection the session
  is attached to.
- On SIGTERM / explicit `quit` command, the daemon extracts the
  changeset and writes it to `--output`, then exits.

Caller workflow:

```
# Terminal 1
sqlite-wasm-run changeset daemon --db data.db --socket /tmp/s.sock --output out.cs &
# Terminal 2 (or a script)
echo "INSERT INTO users VALUES (3, 'carol', 75);" | nc -U /tmp/s.sock
echo "quit" | nc -U /tmp/s.sock
# Daemon writes out.cs and exits.
```

### Pros

- **No architectural surgery.** Phase 2/3's libsqlite3-sys FFI
  wrapping is reused unchanged; the daemon is just a long-lived
  loop on top.
- **No WIT contract changes.** The interface is bytes on a
  socket, evolvable per-version without WIT lockstep.
- **One process, one session, simple lifecycle.** Spawn  use 
  kill  read blob.
- **Captures from EXISTING callers.** Any tool that can write to
  a Unix socket and speak the daemon's protocol  no caller-side
  wasm awareness needed.
- ~200 LOC on top of phase 3. Cost: 1-2 days.

### Cons

- **Two-process UX.** Caller spawns the daemon, talks to it, kills
  it. More steps than "just run my SQL."
- **Socket protocol design.** Needs versioning + error handling +
  some form of framing. Even a tiny protocol carries maintenance
  cost forever.
- **Platform surface.** UDS is Unix-only; Windows needs named
  pipes (different syscalls, different feature parity).
- **Persistence on crash.** If the daemon segfaults or is killed
  uncleanly, the in-flight session is lost. No checkpoint
  capability without designing for it.
- **IPC latency.** ~tens of microseconds per call vs. ns for
  in-process. Material if the caller is sending thousands of
  small statements.
- **Capture is daemon-scoped.** Only changes that go through the
  daemon's connection are captured. If another tool writes the
  same db file via WAL concurrently, those changes are NOT in
  the changeset.

### When it shines

- The callers are EXISTING tools (sqlite3 cli, GUI editors,
  third-party scripts) that don't know about our wasm component
  world. The daemon lets you slot the session capture in between
  them and the db file without modifying their code.
- "Capture for X minutes while my migration runs" workflow 
  spawn daemon, run migration, kill daemon, ship blob.

---

## Option E: WIT-extension architecture

### How it would work

New WIT interface in `sqlite-loader-wit/wit/host-spi.wit`:

```wit
interface session {
    use types.{sqlite-error};
    type handle = u64;

    create: func(dbname: string) -> result<handle, sqlite-error>;
    attach: func(h: handle, table: option<string>) -> result<_, sqlite-error>;
    changeset: func(h: handle) -> result<list<u8>, sqlite-error>;
    delete: func(h: handle) -> result<_, sqlite-error>;
}
```

Host implementation (host/src/lib.rs) backed by libsqlite3-sys
session FFI, keyed by a thread_local `HashMap<handle, *mut
sqlite3_session>` registry.

New wasm extension `extensions/session/` declares SQL scalars:

```
session_create(dbname)   handle (i64)
session_attach(h, table) 0/1
session_changeset(h)     blob
session_delete(h)        0/1
```

Caller workflow:

```sql
.load extensions/session.wasm
WITH h(id) AS (SELECT session_create('main')),
     _   AS (SELECT session_attach((SELECT id FROM h), NULL))
SELECT session_attach((SELECT id FROM h), NULL);

-- make any SQL changes
INSERT INTO users VALUES (3, 'carol', 75);

SELECT session_changeset(<handle>);
SELECT session_delete(<handle>);
```

(Or wrap the boilerplate in a vtab; see the eponymous-vtab pattern
in `completion`, `listargs`.)

### Pros

- **Native SQL interface.** Capture is `SELECT session_changeset(h)`
   no separate process, no socket protocol, no spawn.
- **Composes with SQL.** You can use captured blobs in WITH
  clauses, json_extract, etc. The output isn't outside the db
  world; it's inside it.
- **Multiple concurrent sessions per connection.** Different
  scalars track different tables; everything lives in one
  process state.
- **Reuses the existing extension framework.** Same loader,
  same dispatch path, same WIT contract additions every other
  extension uses.
- **Cross-platform.** Wasm components don't care about the
  host OS  same code on Linux/Mac/Windows.
- **Performance.** ~ns per call, no IPC overhead.
- **Idiomatic.** Matches every other extension's shape ("X is
  an extension; here are its scalars").

### Cons

- **New WIT interface.** Once shipped, lives in the contract
  forever. Versioning concerns; future changes need careful
  back-compat.
- **Plumbing across worlds.** `add_to_linker` wiring needs to
  land in every world that imports `session` (minimal,
  stateful, full, etc.). That's 5+ wiring sites.
- **Handle leakage risk.** If a caller forgets `session_delete`,
  the registry retains a *mut sqlite3_session indefinitely.
  Mitigation: a per-connection cleanup hook in the host. Real
  work but well-trodden territory.
- **Two layers of indirection.** Extension  spi  libsqlite3-
  sys. Marginal performance cost (~100ns extra) for the
  abstraction. Negligible for the use case but worth naming.
- **The "5-7 day" estimate from the original plan.** Real work:
  WIT design + host impl + extension + smoke + cleanup hook
  for handle leaks.

### When it shines

- All SQL goes through OUR cli/runtime (which is the case for
  this project). The wasm component world IS the architecture
  callers live in.
- You want capture to be idiomatic SQL  "what does my caller
  need to know to capture changes?" Answer: load an extension
  and call its scalars. Same shape as everything else they use.
- Building toward a richer session API (patchsets, diff, conflict-
  callback customization). The interface is the right place for
  growth.

---

## Matrix

| Factor | Daemon | WIT extension |
|---|---|---|
| Implementation cost | 1-2 days | 3-5 days |
| Maintenance surface | One subcommand | New WIT interface + extension |
| Caller UX | Two processes | One SQL session |
| Performance / call | ~µs (IPC) | ~ns (in-process) |
| Cross-platform | Unix-first; Windows extra | Yes, any wasm runtime |
| Captures from external tools | Yes | No (only SQL through our cli) |
| Composes with SQL | No | Yes |
| Handle lifecycle | Implicit (process scope) | Explicit (session_delete) |
| Crash recovery | Loses session | Loses session |
| Fits existing architecture | Outlier (IPC, ad-hoc) | Native (matches every other ext) |

---

## Recommendation

Depends on who the caller is.

**Pick the daemon if** the use case is "capture changes made by
EXISTING tools that talk to the db directly (sqlite3 cli, GUI
editors, third-party scripts) without modifying those tools."
The daemon is a supervisor process between them and the db.

**Pick the WIT extension if** the use case is "interactive capture
from sessions that already go through our cli/runtime." The
extension is the idiomatic shape  it matches every other
extension's interface; callers already know how to `.load` and
call scalars. The cost is real architecture (WIT + worlds +
extension + cleanup) but ships an interface that ages well.

For THIS project's likely caller (the cli, scripts driving the
cli, future tools built on top of it), the WIT extension is the
better fit. The daemon makes sense ONLY if there are concrete
external-tool callers asking for it.

**My honest read:** ship neither yet. Phase 3's subcommand model
already covers the "run a SQL script and capture the result" case
which is 80% of the value. The other 20% (interactive capture)
needs a concrete consumer to drive the design  whoever they are
will know whether they want daemon-style or extension-style.
Building either speculatively risks shipping the wrong abstraction.

Defer until a real caller asks.
