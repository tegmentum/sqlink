# PLAN: code-health + tech-debt audit for OSS release

Read-only inventory of the sqlink codebase before public release.
Scope: `core/ cli/ host/ sqlink-httpd/ sqlite-lib/ sqlite-cas-cache/
sqlite-pcache-tvm/ sqlite-mem-tvm/ sqlite-vfs-tvm/ sqlite-embed/`
plus top-level `extensions/` Rust. Excludes `target/`, generated WIT
bindings, and `extensions/_shared-target/`.

Audit date: 2026-06-22, branch `main` at cc6924f (PLAN reorg). All
findings are file:line citations — no code changes proposed inline.

---

## Headline finding (the one thing)

**`host/src/lib.rs` is 10,189 lines.** It owns ~32% of all Rust LOC
in the project, holds 91 of the 252 `.unwrap()/.expect()/panic!()`
sites (36%), 125 of 271 `.clone()`s (46%), and contains 5 of the 10
longest functions in the codebase. Every other quality finding in
this report is dominated by this one file. **Splitting it into ~8
sub-modules** (the WIT-world `impl` blocks already form natural
seams — see §5) is the single highest-leverage change in this
audit. Size **L (3-5 days)**. Impact: makes every subsequent
refactor in this file ~3x cheaper; opens the door to extracting
`vtab.rs`-style siblings for sessions / spi / dispatch / blob-cache.

---

## One-page summary — top refactors by leverage

| # | Refactor | Size | Impact |
|---|----------|------|--------|
| 1 | Split `host/src/lib.rs` (10k LOC) into per-world `impl` files | L | Opens the path to every other host refactor; internal but unblocks reviewers |
| 2 | Add CI jobs for `cli`, `sqlink-httpd`, `sqlite-lib`, and the wasm32-wasip2 build | M | Every user — currently CI only tests host + cache + compose |
| 3 | Replace 32× `expect("ensured open")` with a `with_spi_conn(\|c\| ...)` helper (like the L2a `with_user_conn` shape we just shipped) | S | Eliminates an entire class of panic-on-invariant in host's hot path; internal cleanup |
| 4 | Sweep top 10 longest functions in `host/src/lib.rs` + `cli/src/lib.rs` — they range 90-630 lines; most have clean internal seams | M | Internal; review-quality boost |
| 5 | Audit + consolidate the 303 `.map_err(\|e\|...)` boilerplate sites with a `From<E>` impl or thiserror crate-wide error types | M | Internal; cleanup |
| 6 | `extensions/`: 229 crates × ~3 file Cargo.toml — feature audit + Cargo.lock cleanup pass | M | OSS users see crate count; relevant for crates.io publish story |
| 7 | Cli's `do_load` (378 lines) + `do_cache` (248) + `do_compose` (212) need extraction — each is a state machine masquerading as a fn | M | Internal review-quality |
| 8 | `sqlink-httpd`, `sqlite-lib`, `sqlite-embed`, `cli`, `core`: zero CI test coverage today (see §9) | M | Every user |
| 9 | Workspace dep deduplication — `serde`, `serde_json`, `tokio` declared 4-5× with subtly-different version strings (`"1"` vs `"1.0"`) | S | Build hygiene |
| 10 | Top-level `host/Cargo.toml` is 121 lines with 89 `embed-*` features — extract feature wiring into a generated file from the survey DB | S | Build hygiene |

---

## §1 — TODO/FIXME/XXX/HACK census

**Real action items (the only one):**

- `host/src/lib.rs:4128` — `// TODO: gate by policy.fs once a filesystem capability lands` — relevant for OSS. Should be dispositioned: ship the filesystem capability OR drop the path that this comment guards. Needs a one-paragraph plan; track as a follow-up.

**False positives** (`XXX` appearing as a literal in extension data strings, not markers):

- `extensions/ssn/src/lib.rs:89` + `src/embed.rs:69` — `XXX-XX-{last4}` is the literal SSN-mask format string.
- `extensions/bic/src/embed.rs:30,87,92`, `extensions/bic/src/lib.rs:53,169,174` — `"XXX"` is the BIC code for "primary office" per ISO 9362.
- `extensions/core-dotcmd/src/lib.rs:156` — `"Run Time: real X.XXX"` is a `.timer` output format string.
- `cli/src/settings.rs:61` — same `Run Time: real X.XXX` string in `.timer` docs.

**Verdict:** the codebase is remarkably clean on this front — 1 real
TODO, 9 false-positive grep hits.

---

## §2 — Panic audit

Distribution of `.unwrap() / .expect(...) / panic!()` (excluding
test files, excluding `target/`):

| Crate | Count | Note |
|-------|------:|------|
| host | 160 | by far the worst; 91 in `lib.rs` alone |
| core | 64 | mostly test-helper paths inside `db.rs`; 4 real `panic!()` |
| sqlite-vfs-tvm | 12 | almost entirely test-only |
| sqlite-pcache-tvm | 9 | mostly test-only |
| sqlite-lib | 4 | needs inspection |
| sqlink-httpd | 3 | low |
| cli | **0** | clean (already returns Strings from every dot-cmd) |
| sqlite-cas-cache, sqlite-mem-tvm, sqlite-embed | 0 | clean |

**Worst offenders — top 20 in `host/src/lib.rs`:**

| Line | Pattern | Verdict |
|-----:|---------|---------|
| 2907, 2937, 2961, 2977, 2988, 3026, 3049, 3060, 3080, 3096, 3111, 3277 | `r.as_ref().expect("ensured open")` | **(a) invariant** — `shared_spi_ensure_open(self.host)?` is called immediately before. Safe but verbose. Worth a closure helper (see §summary #3). |
| 4474 | `self.entries.remove(pos).unwrap()` | **(a) invariant** — `pos` came from a same-thread `iter().position()` 4 lines up. Safe. |
| 4814 | `g.as_ref().map(\|(_, c)\| c).expect("just-opened")` | **(a) invariant** — the L2a `with_user_conn` just-opened the connection. Safe. |
| 5811, 5921, 5937, 5946, 5958, 5968 | `guard.as_mut().unwrap()` | **(a) invariant** — `stateful_locked` returns Result on miss; the Option-unwrap inside is post-`?`. Worth `.as_mut().ok_or_else(...)` for clarity. |

**`panic!()` sites — 10 total across non-test code:**

| File:Line | Comment | Verdict |
|-----------|---------|---------|
| core/src/db.rs:1903 | `_ => panic!("expected row")` | inside `#[test]` — safe |
| core/src/db.rs:2075,2093,2129 | `let Value::Integer(n) = ... else { panic!() }` | inside `#[test]` — safe |
| host/src/compose_provider.rs:487,511,528 | `_ => panic!()` inside `convert_*` value variants | **(b) potential bug** — these should be `Err(InvalidVariant)` rather than panic. Reachable via malformed WIT input. **Needs deeper look.** |
| host/src/cache.rs:338 | `StepResult::Done => panic!("no row")` | inside `#[cfg(test)]` — safe |
| sqlite-pcache-tvm/src/cache.rs:621,625 | `panic!("page 1 should be in shadow ...")` | inside `#[test]` — safe |

**Bottom line:** ~95% of panic-sites in this codebase are
invariant-driven and not real bugs. The 3 `compose_provider.rs`
sites are the only real flags; the rest of the cleanup is
ergonomic (shorter, intention-revealing helpers vs raw `expect`).

---

## §3 — Dead code

**Native build (`cargo build --release`):** 0 dead-code warnings.

**Wasm build (`cargo build -p sqlite-cli --target wasm32-wasip2 --release`):**

| Symbol | Location | Verdict |
|--------|----------|---------|
| `use bindings::sqlite::extension::types::SqlValue` | `cli/src/lib.rs:783` | **delete** — local import, never referenced. Likely a leftover from the Stage 5 cleanup. |
| `fn log_event` | `cli/src/lib.rs:910` | **delete** — function defined, never called. Likely a leftover from `.log` migration. |
| `Err(String)` variant | `cli/src/dot.rs:39` (`FetchResult`) | **needs deeper look** — possibly used only via `Display` formatting; check before deleting. |
| `priority: i64` | `cli/src/sqlink_registry.rs:41` (`ResolverRow`) | **needs deeper look** — could be a column we read from SQL but never bind in code. If so, swap for `_priority` or drop the field. |

**`#[allow(dead_code)]` annotations — 3 sites:**

| Location | Verdict |
|----------|---------|
| `host/src/lib.rs:5248` | needs inspection (single line, no surrounding fn shown in summary) |
| `sqlite-lib/src/lib.rs:790` (`pub fn _touch`) | **intentional** — leading underscore marks it as a type-checker pin |
| `sqlite-pcache-tvm/src/cache.rs:143` | needs inspection |

---

## §4 — Code-smell patterns

### Functions > 100 lines

**`host/src/lib.rs` top 10:**

| Lines | Start | Function |
|------:|------:|----------|
| 630 | 2228 | `unsafe fn register_host_embedded_extensions` — auto-generated dispatch over 89 `embed-*` features; **leave alone** (mechanical, code-genny shape) |
| 326 | 747 | `fn manifest_for_ext` — 4-deep nested transform of WIT manifests into bindings. **Split candidate.** |
| 258 | 4173 | `fn refresh_call_budget` — needs deeper look |
| 191 | 5580 | `async fn register_component` — split into validate / instantiate / store phases |
| 132 | 5865 | `pub async fn dispatch_scalar` — already has 3-engine Store routing; manageable |
| 132 | 4640 | `pub fn new` (`Host::new`) — long constructor, split candidate |
| 97  | 1232 | `async fn resolve` — fine |
| 94  | 5771 | `pub async fn dispatch_dot_command` — fine |
| 90  | 1124 | `async fn handle` (http) — fine |
| 88  | 1558 | `unsafe fn register_host_dot_command_function` — fine (FFI shape) |

**`cli/src/lib.rs` top 10:**

| Lines | Function |
|------:|----------|
| 378 | `do_load` — state-machine; pre-flight / trust gate / TOFU / register-N-things / format. **Split into stages.** |
| 248 | `do_cache` — multi-subcommand dispatch (`stats` / `gc` / `verify`); each subcommand is a function in disguise |
| 212 | `embed_core_dotcmd` — startup auto-embed; mostly include_bytes! glue, fine as-is |
| 172 | `eval_input` — main dispatch loop; touching this is risky. Leave for last. |
| 146 | `build_cli_state_snapshot` — splits cleanly by namespace (general / params / conn) |
| 135 | `run` — the main loop; split candidate |
| 104 | `do_compose` — manageable |
| 90  | `is_statement_complete` — sqlite3_complete replacement; intentional shape |

### Files > 1500 lines

| LOC | File | Verdict |
|----:|------|---------|
| **10,189** | `host/src/lib.rs` | Split — **the headline finding** |
| 2,833 | `cli/src/lib.rs` | Extract dot-cmd `fn do_*` helpers into `cli/src/dotcmds/*.rs` |
| 2,201 | `core/src/db.rs` | Could split into `connection.rs` / `stmt.rs` / `vfs.rs` / `aggregate.rs` |
| 1,544 | `host/src/vtab.rs` | Already a sub-module of host; could split sqlite3_module trampolines from registry |
| 1,445 | `sqlite-embed/src/lib.rs` | Tight, mostly typed wrappers; leave |

### Boxed closures

Only 2 sites total — `core/src/db.rs:287,551` for the `set_stmt_trace`
trampoline. Not hot paths. **Leave alone.**

### `String::from(format!())`

Zero occurrences in scope. **Clean.**

### `.map_err(|e| ...)` boilerplate

**303 occurrences across the codebase.** Top files:

| Count | File |
|------:|------|
| 132 | `host/src/lib.rs` (anyhow conversion mostly) |
| 22 | `host/src/main.rs` |
| 19 | `host/src/component_blob_cache.rs` |
| 6 | `sqlink-httpd/src/tls.rs` |
| 3 | `sqlink-httpd/src/main.rs` |
| 2 | `sqlink-httpd/src/wasm.rs` |
| 1 | `host/src/cache.rs` |

Most are `.map_err(|e| anyhow!("...: {e}"))`. A crate-level
`thiserror`-style error type with `#[from]` impls would eliminate
~80% of these mechanically. **Size M (1-3 days).**

### `.clone()` heatmap

| Count | File |
|------:|------|
| **125** | `host/src/lib.rs` |
| 18 | `cli/src/lib.rs` |
| 12 | `sqlite-lib/src/lib.rs` |
| 12 | `sqlink-httpd/src/main.rs` |
| 11 | `host/tests/load.rs` (test code) |
| 10 | `host/src/main.rs` |
| 7  | `sqlink-httpd/src/router.rs` |
| 7  | `host/src/compose_provider.rs` |
| 6  | `cli/src/orchestration.rs` |
| 5  | `sqlink-httpd/src/wasm.rs` |

The 125 in `host/src/lib.rs` are mostly `name.clone()`,
`ext_name.clone()`, `path.clone()` — strings cloned to pass into
`tokio::spawn` or to use after a borrow. Could be reduced ~30% with
`Arc<str>` for the most-cloned identifiers (extension name,
db_path). **Size M; perf gain is small (these aren't hot paths) but
the readability gain is real.**

---

## §5 — Module bloat — where `host/src/lib.rs` could split

The file already has natural seams via `impl` blocks. Suggested split:

| Proposed file | Contains | LOC estimate |
|---|---|---:|
| `host/src/policy.rs` (exists, expand) | `LoadedState::http`, `dns` impls | ~300 |
| `host/src/spi_impl.rs` | `loaded::sqlite::extension::spi::Host`, `bindings::...::spi::Host`, `with_user_conn` + helpers, the 32 `expect("ensured open")` sites | ~1500 |
| `host/src/aggregate.rs` | `HostLoadedAggregate` + `dispatch_aggregate_*` impls | ~600 |
| `host/src/dot_dispatch.rs` | `dispatch_dot_command` + `sync_dispatch_dot_command` + the `dot_command()` SQL fn registration | ~400 |
| `host/src/session_impl.rs` | `session::Host` impl | ~250 |
| `host/src/component_load.rs` | `register_component`, `load_extension_from_*`, `resolve_uri_to_bytes` | ~1200 |
| `host/src/manifest.rs` | `manifest_for_ext` + WIT manifest conversions | ~500 |
| `host/src/dispatch.rs` | `dispatch_scalar / aggregate_* / collation / authorize / on_update / on_commit / on_rollback / vtab_*` | ~2500 |
| `host/src/lib.rs` (residue) | `Host` struct, `Host::new`, `bindings!` macro, world declarations, top-level entry points | ~1500 |

That's a **deliberate ~5-day refactor** that the existing test suite
should cover, but it touches every line in the file. Best done as
one PR with reviewer pre-approval; don't sprinkle across many small
commits.

---

## §6 — Cargo.toml audit

### Workspace declared deps with subtle drift

| Dep | Variants seen |
|-----|---------------|
| `tokio` | `tokio = { version = "1", features = [...] }` (2 different feature lists, host vs httpd) |
| `serde` | `version = "1.0"` (host) vs `version = "1"` (httpd, handlers) |
| `serde_json` | `"1.0"` (host) vs `"1"` (4 other places) |

**Fix:** workspace.dependencies = { ... } at root, all members
`{ workspace = true }`. **Size S.**

### `host/Cargo.toml` is 121 lines, 89 `embed-*` features

All 89 are wired into `register_host_embedded_extensions` (verified
by grep — 0 unused). The feature list could be auto-generated from
the survey DB (`provenance/extensions.db`) instead of hand-maintained,
but that's a chore not a blocker. **Size S, low priority.**

### Workspace member list

`Cargo.toml` lists 20 workspace members. 9 extensions are
workspace members; the other 220 are standalone with their own
`[workspace]` block. Mixing both modes works but means
`cargo build --workspace` from the root only builds the listed
ones. Probably right (the 220 standalone are released individually)
but worth documenting in CONTRIBUTING.

### Per-crate dep audit (would each crate compile if I removed deps not used by `use`)?

**Not done in this pass — needs `cargo machete` or `cargo-udeps`.**
Note as a follow-up; estimate ~1-2 unused deps per crate × 10
crates = 10-20 trims, **size S total**.

---

## §7 — Bonus findings

### Test coverage gap

| Crate | Integration tests | Inline `#[test]` files | Tested in CI? |
|---|---:|---:|---|
| host | 12 | 3 | ✓ |
| sqlite-cas-cache | 4 | 0 | ✓ (cache-tests job) |
| sqlite-pcache-tvm | 1 | 1 | ✗ |
| sqlite-mem-tvm | 1 | 1 | ✗ |
| sqlite-vfs-tvm | 1 | 2 | ✗ |
| core | 0 | 1 | ✗ |
| cli | 0 | 0 | ✗ |
| sqlink-httpd | 0 | 0 | ✗ |
| sqlite-lib | 0 | 0 | ✗ |
| sqlite-embed | 0 | 0 | ✗ |

**CI runs only 3 jobs** (host-checks, cache-tests, compose-tests).
**No CI job builds the wasm32-wasip2 cli or any extension.** Adding:

- A `wasm32-wasip2-build` job that does `cargo build -p sqlite-cli --target wasm32-wasip2 --release` and verifies the component-encoding step (`wasm-tools component new`)
- A `cli-smokes` job that runs the existing `examples/sqlite-utils-tour.sql` through the built cli
- A `sqlite-lib-tests` job (lib smoke + compose test)

…would cover the major surface gaps. **Size M (1-2 days).**

### Recently surfaced issues from this session worth re-checking

- `host/src/lib.rs` line 4128 TODO about filesystem capability gating.
- 3 `panic!()` sites in `host/src/compose_provider.rs` (lines 487, 511, 528) — see §2.
- `Err(String)` variant in `cli/src/dot.rs:39` and `priority: i64` in `cli/src/sqlink_registry.rs:41` — see §3.

### Not investigated this pass (would each take >30min)

- Cargo `[features]` cross-graph audit (which features force which deps, and are any cycles introduced)
- Async borrow patterns (`Arc<RwLock>` vs `Arc<Mutex>` vs `parking_lot` choices) in host — there are at least 3 styles co-existing
- `core::db::Connection` lifetime / `Sync+Send` story — the `ReentrantMutex<RefCell<Option<Connection>>>` is novel; deserves a design-note callout in `host/SPI.md`
- License/attribution audit for embedded SQLite code under `deps/`
- Auditing the 229 extensions for anything sensitive (URLs to private services, etc.) — sister forks have touched this for tegmentum but a fresh pass is worth it

---

## Sequencing recommendation

A reasonable order to ship these (each step independently
verifiable, each lands as its own PR):

1. **§3 dead-code cleanup** (a few minutes; just delete the 2-4 unused things, gives a clean baseline)
2. **§9 workspace dep deduplication** (size S)
3. **CI expansion: add 3 missing jobs from §7** (size M, foundational — lets every subsequent change get tested)
4. **§1 — disposition the L4128 TODO** (decide ship-vs-drop)
5. **§2 — the 3 `compose_provider.rs` panics → Result** (size S, real bug fix)
6. **§3 / `with_spi_conn` helper** (size S, eliminates 32 expects)
7. **`host/src/lib.rs` split** (size L, the headline finding)
8. **§4 — top 5 long functions in cli + host** (size M)
9. **§5 — `.map_err` consolidation via thiserror** (size M, ergonomic)
10. **§6 — host embed-* feature generator** (size S, optional)

Items 1-3 are the launch blockers; everything else is post-OSS-launch
polish.
