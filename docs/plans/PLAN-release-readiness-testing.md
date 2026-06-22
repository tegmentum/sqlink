# PLAN: release-readiness testing audit

Read-only inventory of test coverage across sqlink, with a
ranked list of the highest-value additions to make before
flipping the OSS switch. Pure assessment; no code changes.

## TL;DR — top 5 additions for shipping confidence

| # | Addition | Effort | Catches |
|---|---|---|---|
| 1 | **End-to-end `.load <ext>` integration test in host/tests/** that builds one real extension component (case or stats), loads it, exercises a scalar / aggregate / vtab, then `.unload`s. **No such test exists today.** | M | A whole-pipeline regression — wit-bindgen drift, store routing, host trampoline wiring, `.load` security gate, `.unload` teardown. Anything that breaks loaded-extension dispatch lands here, not in user reports. |
| 2 | **`tests/cli/sqlite-wasm-wrapper.sh` rename + audit pass.** The path still says `sqlite-wasm-wrapper.sh` post-rebrand; the script tests `./sqlink`. Same for `tests/cli/test_load.sh` and `test_commands.sh` — confirm they execute and still assert correctly against the renamed binary + new dot-command surface. | S | Stale shell smokes that look green because they're never run. |
| 3 | **Property tests on identifier quoting in `utils-schema` / `utils-data`.** 35 + 24 quote-helper call sites between them; one unicode/embedded-quote bug = SQL injection or schema corruption. `proptest` against the helper trio with arbitrary table/column names. | M | Quoting bugs that escape table-name-with-quote or sqlite-keyword-as-identifier. The most likely class of "embarrassing OSS day-1 bug." |
| 4 | **Component cache hit/miss/evict cycle test.** Zero tests cover `try_c2_lookup` / `try_c2_store` / `evict_to` in host/. The cache is on the hot `.load` path; a regression silently doubles cold-start time. | S-M | Cache invalidation bugs after L2a's `user_conn` change; HMAC-rotation edge cases; LRU eviction off-by-one. |
| 5 | **Session changeset roundtrip integration test.** Stage 6 shipped the WIT + session-cli extension but `host/tests/` has zero coverage of `session_create` → INSERTs → `changeset` → bytes-non-empty → `delete`. Smoke exists ad-hoc in the rebrand handoff but isn't a CI gate. | S | A real-world data-sync bug shipping unnoticed; sqlite3session API has subtle PK-vs-no-PK rules. |

**Headline gap:** there is no automated test anywhere that loads a real `.wasm`
extension component end-to-end and invokes one of its scalars / aggregates /
vtabs. The host has 27 integration tests, but every single one mocks or
sidesteps the actual wit-bindgen dispatch surface. That's the test most
likely to catch a release-blocker.

## 1. Test inventory

| Crate | Unit `#[test]` | Integration `tests/` | Notes |
|---|---:|---:|---|
| `core/` | 13 | 0 | sqlite3 wrapper; covers Connection/Stmt basics |
| `cli/` | **0** | **0** | The cli has zero Rust-level tests. All coverage is the shell smokes in `tests/cli/` |
| `host/` | 17 | 27 (12 files) | The deepest test surface; covers loader, auth, ed25519 trust, sqlite-lib composition, TVM probes |
| `sqlink-httpd/` | **0** | **0** | One `handlers/auth/smoke.sh` shell exists; nothing else |
| `sqlite-lib/` | **0** | **0** | Public API; only covered transitively via `host/tests/sqlite_lib.rs` |
| `sqlite-cas-cache/` | 0 | 41 (4 files) | Best-tested non-host crate — eviction / migration / resolver / store |
| `sqlite-pcache-tvm/` | 6 | 3 (1 file) | TVM substrate; capacity tests |
| `sqlite-mem-tvm/` | 5 | 3 (1 file) | Same shape |
| `sqlite-vfs-tvm/` | 10 | 3 (1 file) | Same shape |
| `sqlite-embed/` | **0** | **0** | Macro/codegen helper for embedded extensions; no tests |
| `extensions/` | 317 across 52 dirs (of 228 total) | n/a | 23% of extensions have at least one test |

**External smokes:**

- `tooling/smoke.py` — extension smoke driver (Python)
- `tooling/cli-smoke.py` — cli smoke driver
- `tooling/bench.py` — benchmark harness, emits markdown for `docs/plans/PLAN-benchmarks.md`
- `tooling/check-snippets.py` — extracts + verifies code snippets
- `tooling/doc-refs-check.py` — verifies doc cross-references
- `tooling/scaffold.py` — extension scaffolder (no tests for it)
- `examples/sqlite-utils-tour.sql` — 30 lines, 29 dot-command invocations, 28 distinct commands
- `scripts/sqlite-utils-tour.sh` — runs the .sql end-to-end against a fresh db
- `tests/cli/{sqlite-wasm-wrapper,test_commands,test_load,test-cli}.sh` — 894 lines total of bash smokes
- `tests/integration/jco/*.mjs` — 1627 lines of JS-side jco tests against the wasm cli

## 2. Coverage gaps by crate

### host/ (the deepest surface; matters most)

  - `host/src/lib.rs::Host::dispatch_vtab_update` (L6637): zero tests. vtab UPDATE/DELETE path can silently corrupt data without anyone noticing. The 20 `dispatch_vtab_*` methods (L6164-L6742) are uncovered as a group.
  - `host/src/lib.rs::Host::try_c2_lookup` / `try_c2_store` (post-L2a): no hit/miss/evict cycle test. The cache is the only thing standing between cold-start and 30s component re-parse.
  - `host/src/lib.rs::Host::dispatch_aggregate_value` / `dispatch_aggregate_inverse` (L5391, L5416): window-aggregate path. The `stats` extension uses these but no test asserts xValue / xInverse fire in order.
  - `host/src/lib.rs::sqlite_code_to_auth_action` (L2188): 33-arm match. One mistranslation = a SECURITY-relevant authorizer call routes to the wrong action. No table-driven test.
  - `host/src/lib.rs::Host::session_create` and the entire session interface (Stage 6, L8580+): zero tests. Smoke happened in handoff conversation only.
  - `host/src/component_blob_cache.rs::evict_to` LRU bound: not tested.
  - `host/src/cache.rs::SqliteCasStore::lookup_by_hash`: blake3-only-after-blake3-hits fast path (L2b plan): claims "already true" but no test asserts the second SELECT doesn't fire.
  - `host/src/lib.rs::Host::run_wasm` Policy enforcement: deny_all path tested once (runnable_sqlite_demo); per-capability denial (http, dns, http-headers) untested.

### cli/ (zero Rust tests)

  - `cli/src/lib.rs::is_statement_complete` (L292): the 130-LOC SQL completeness parser. Replaced sqlite3_complete in Stage 5f; tests cover only the comment-fix smoke. Edge cases: BEGIN/END trigger bodies, nested block comments, multi-line strings with embedded escapes.
  - `cli/src/lib.rs::skip_leading_trivia` (L253): the comment-stripping helper that fixed the sharp-edge bug. Test would be 5-line proptest with arbitrary mix of `--`, `/* */`, whitespace, dot-command.
  - `cli/src/lib.rs::do_help` (L581): walks the loader registry. No test for "extension owns command X" lookup correctness.
  - `cli/src/lib.rs::eval_input` (L409): dispatcher. Routing precedence between dot-command / `.session` stub / loader-fallthrough has no regression test.
  - `cli/src/lib.rs::try_db_registry_resolve` (L656): Phase 4 CAS walk. Fallthrough to sqlink_cas_resolver rows untested.
  - The 73 dot commands are exercised by `examples/sqlite-utils-tour.sql` (28 of 73 = 38%). 45 dot commands have no automated invocation anywhere.

### sqlite-lib/ (zero tests; covered only via host transitively)

  - Public API — `execute` / `execute_scalar` / `execute_batch` — has no direct unit tests. host's `sqlite_lib.rs` exercises it via composed runnable but that's an integration vehicle, not a unit-level guard.
  - Error mapping `core::db::Error` → spi `SqliteError`: no test.

### core/ (13 unit tests; thin)

  - Tests exist for Connection/Stmt happy paths; error paths are 40 `Err()` constructions in src with 2 error-assertions in tests.
  - `update_hook` / `commit_hook` / `rollback_hook` callbacks tested only via `host/tests/load.rs`'s register-then-fire shape.

### sqlink-httpd/ (zero tests)

  - Route dispatch (`router.rs`): zero coverage.
  - The 5 handler crates (echo, registry, sql, auth, yaml-to-json, markdown): one shell smoke for auth; nothing else.
  - TLS termination + cert reload: untested.
  - `wasm.rs` language-runtime wiring: untested.

## 3. Error path coverage (sample)

| Crate | `Err(...)` constructions in src | Error-assertions in tests | Coverage estimate |
|---|---:|---:|---|
| `host/` | 272 | 4 (across all of host/!) | **~1-2%** |
| `cli/` | 89 | 0 (zero tests at all) | **0%** |
| `core/` | 40 | 2 | ~5% |
| `sqlink-httpd/` | 20 | 0 | **0%** |
| `sqlite-lib/` | 9 | 0 | 0% |

The `host/` gap is striking: 272 places that build an `Err(...)` and 4 tests
that assert against any of them. Most error variants are "this should be
impossible if invariants hold" — fair enough — but a clutch of them (Policy
denials, loader errors, capability violations, session ffi rc-non-zero) are
real branches a malicious or malformed input takes.

## 4. Integration test gaps

| Flow | Status | One-line gap |
|---|---|---|
| `.load <ext>.wasm` of a real component | **No automated test** | Build case+stats+vec0 in CI, load each, assert one scalar/agg/vtab call works |
| `.unload <ext>` cleanup | **No test** | After unload, re-resolving the function should yield `no such function` |
| `.session create→insert→changeset→delete` | **No test** | Stage 6 shipped without a CI gate |
| Component cache hit / miss / LRU evict | **No test** | Cold + warm load timing assertion; evict cap exercise |
| 73 dot commands surface | 28 of 73 in tour (38%) | Either extend the tour or accept the legacy `tests/cli/*.sh` smokes cover the gap (verify post-rebrand) |
| `.open` swap between dbs | **No test** | After `.open new.db` the host's spi conn + user_conn should both rotate; assert reads from old db's tables fail |
| `.trace` + `.auth` toggle observation | **No test** | Stage 5e.8/5e.9 shipped without CI smoke |
| sqlink-httpd handler dispatch | **One shell** | Only `handlers/auth/smoke.sh` exists; sql/echo/registry/markdown/yaml-to-json paths uncovered |
| TLS cert reload (SIGHUP) | **No test** | Documented behavior; not asserted |
| `--trust=prompt` mode (L3a, just shipped) | **No CI test** | Manual stdin smokes from the L3a handoff are not automated |
| `describe_extension_from_uri` non-`file:` schemes (L3b) | **No test** | The new resolver-walking path isn't exercised |
| Capability deny enforcement (http allowlist, dns allowlist) | **No test** | Policy says X is denied; no test loads an X-using ext under deny |
| HMAC-key rotation / missing-key degrade-to-no-cache | **No test** | L3c shipped a diagnostic; no test triggers it |

## 5. Smoke harness audit (post-rebrand)

- `tooling/smoke.py`, `tooling/cli-smoke.py`, `tooling/bench.py`: **clean** — no remaining `sqlite-wasm` references. Verified by grep.
- `tooling/plan-add.py`: updated for the `docs/plans/` move; verified by grep of `PLAN-sqlite-plugins.md` path.
- `tests/cli/sqlite-wasm-wrapper.sh`: **filename is stale**. The first line says "Integration test for the ./sqlink shell wrapper" — content matches the rebranded binary but the file path still carries the old name. Either rename the file or accept the legacy.
- `tests/cli/test_load.sh`, `test_commands.sh`, `test-cli.sh`: content not audited in this pass; line counts non-trivial (114 / 473 / 215). Worth a manual run to confirm they still pass post-rebrand.
- `tests/integration/jco/*.mjs`: 1627 lines of JS-side integration tests. Their `package.json` hasn't been touched by the rebrand; need a confirmation run.
- `examples/sqlite-utils-tour.sql` + `scripts/sqlite-utils-tour.sh`: shipped at Stage 5 (1ce7298); should still pass against the post-rebrand cli — verify.

**Action:** one bash session that runs each shell smoke explicitly and reports
pass/fail. None of these are wired into CI today (see §7).

## 6. Property / fuzz test opportunities

1. **SQL identifier quoting**: `extensions/sqlite-utils-{schema,data,maint}` have 74 combined `quote_ident` / `safe_ident` / `sql_string_lit` call sites. `proptest` with arbitrary unicode strings — including embedded `'`, `"`, `;`, sqlite-keywords — would catch any helper bug that lets a hostile table/column name break out into the surrounding SQL. **Highest ROI by far** — affects every data-ingest path.
2. **`is_statement_complete`** in cli: the 130-LOC Rust replacement for `sqlite3_complete`. Property test: for any input string, `is_statement_complete(s) == sqlite3_complete(s)` (if a libsqlite3 reference were available in tests) or against a golden set of statements. Catches misclassified strings/comments/BEGIN-END.
3. **Auth-action code mapping** (`sqlite_code_to_auth_action` in host): table-driven test over every `SQLITE_*` action constant. Not strictly fuzz but the same vibe — catches the 1-in-33 chance someone shuffled the arms.
4. **Policy / capability deny path**: `proptest` over Policy struct mutations + a fixed extension, assert that the loader respects each constraint. Currently zero property coverage.
5. **CAS digest fallback (blake3 → sha256)**: input arbitrary bytes, both algorithms compute the digest, store under blake3, lookup-by-blake3 returns Some, lookup-by-sha256 returns Some, lookup-by-random returns None. Property over the value space. Catches index bugs and the L2b "wasted second SELECT" assertion.

## 7. CI gaps

Current `.github/workflows/ci.yml` runs:

- `host-checks`: `cargo fmt --check` + `cargo clippy --all-targets --all-features` + `cargo test --all-features` (host crate only)
- `cache-tests`: `cargo test --lib cache` against host
- `compose-tests`: `cargo test --lib compose_provider` against host

**Not in CI:**

- `core/` `cli/` `sqlink-httpd/` `sqlite-lib/` `sqlite-embed/` tests — none of these crates are even built in CI
- `sqlite-cas-cache/` integration tests (41 tests across 4 files) — not exercised
- The TVM crates' substrate probes (`tvm_*_probe.rs`)
- The wasm32-wasip2 build of the cli — none of the wasm side is built
- `tests/cli/*.sh` (894 lines of bash smokes)
- `tests/integration/jco/*.mjs` (1627 lines of JS smokes)
- `examples/sqlite-utils-tour.sql` (no CI invocation)
- Workspace-wide `cargo fmt --check` / `cargo clippy` (only host is checked)
- Doc build (`cargo doc --no-deps`) — would catch broken intra-doc links
- `tooling/doc-refs-check.py` — already exists, not wired
- Submodule (`sqlite-loader-wit/`) fmt+clippy+tests
- Coverage report — no tarpaulin / llvm-cov
- License-header check (LICENSE shipped 6fe4f81; no enforcement of per-file headers)

The ci.yml itself says "Future jobs (held until the wasm-side build is moved into CI)" — that's the single biggest gap. Cold-start cost is real (~150 MB wasi-sdk install per run + cargo-component install) but cacheable.

## 8. Recommended next moves

1. **Fix the ci.yml expansion**: build cli + 3 reference extensions on wasm32-wasip2, run the tour script, run the `tests/cli/*.sh` smokes. M effort; cache the wasi-sdk.
2. **Add host/tests/loaded_extension_smoke.rs**: build case + stats + vec0 in the test's build step (via `cargo build -p ... --target wasm32-wasip2`), `host.load_extension_from_path()`, exercise one of each kind, assert correctness. This is the #1 headline gap.
3. **Add proptest to extensions/sqlite-utils-schema**: just the quote_ident + sql_string_lit helpers. ~50 LOC, catches the embarrassing OSS day-1 bug class.
4. **Wire the existing shell smokes into CI**: `tests/cli/*.sh` exists; running it adds zero new test code, just a new CI job that probably reveals 2-3 stale assertions to fix.
5. **Workspace-wide fmt/clippy**: bump the host-only check to `cargo fmt --all --check` + `cargo clippy --workspace --all-targets`. Will surface lint debt across the 10+ in-repo crates that have never been linted.

## Out of scope for this audit

- Refactoring opportunities (the user mentioned them in the same prompt; separate pass)
- Performance / optimization (separate pass — `tooling/bench.py` is the entry point)
- Doc completeness audit (`tooling/doc-refs-check.py` would surface that)
- Security review (`security-review` skill exists for that)
