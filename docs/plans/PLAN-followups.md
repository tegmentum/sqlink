# Plan: v1 follow-ups — roadmap for outstanding post-v1 work

## Status (2026-06-25)

Roadmap for outstanding work after the v1 shipping push (bundles +
prefixes + cas-cache + cli + tests + CI + fuzz/mutation infra all
landed and pushed to origin/main). Single document covering many
small-to-medium follow-ups because each is too small for its own
PLAN doc but the total picture (effort + sequencing + dependencies)
matters for pacing.

## Motivation

Multiple individual PLAN docs (PLAN-bundles, PLAN-prefixes,
PLAN-cas-cache, PLAN-wal-archive) each captured a single feature
end-to-end. Several smaller items surfaced during their
implementation got noted as "v1.1" or "deferred" in the source
docs. This roadmap consolidates them so:

- The total post-v1 effort is visible in one place.
- Priorities are explicit (not just "v1.1" without ranking).
- Dependencies between items are surfaced.
- Fan-out structure is clear when several can run in parallel.

This is NOT a feature plan; each item below references its
originating PLAN-*.md or commit for the full context. Items here
get a one-paragraph summary + effort + dependencies + scope cut.

## Priority + sequencing

```
P0 (blocking)         P1 (high value)             P2 (medium)              P3 (nice-to-have)
─────────────────────────────────────────────────────────────────────────────────────────
CI green-up    ───►   Prefix v1.1 sweep    ───►   Single-slot hook       More fuzz targets
                      Live .prefix prefer         audit recording        Longer fuzz runs
                      Cosmetic warnings           install_loaded_ext
                                                  refactor
                                                  Bundles browser
                                                  dispatch driver
                                                  Bundle multi-aliasing
                                                  .load auto-cache
                                                  HTTPS resolver mock
                                                  Hostile-fs fixtures
                                                  Migration-arm tests
                                                  cargo-mutants re-run
```

P0 blocks everything because a red CI hides real regressions in
subsequent work. P1 items have high-value + low risk + no
dependencies on each other and can fan out in parallel. P2 items
mostly group around shared infrastructure (test harnesses) or
shared file regions; some can parallelize.

## P0: CI green-up

### Background

The recent push triggers (`8f82db5` prefixes hot-path merge, prior
`241008b` compression-multiplexer + `cca5735` cargo fmt fixes) all
went red on real GitHub Actions despite the local act-based
verification. Three distinct things broke at different runs:

- `cargo fmt --check` syntax (fixed in cca5735, then regressed
  somehow — need to check).
- `CARGO_BUILD_TARGET: ""` corrupting `cargo install` (fixed in
  cca5735).
- Workspace manifest load failing on missing
  compression-multiplexer (fixed in 241008b by publishing +
  submoduling).

Post-fix push (`8f82db5`) still shows `failure` in 17s on real CI.
Likely a fourth issue — possibly the prefix substrate's 217-file
Manifest{} sweep introduced fmt drift OR a workspace member dep
that wasn't covered by my earlier excludes.

### Scope

- Pull `gh run view <ID> --log-failed` on the failing run to see
  the actual error.
- Fix whatever surfaces. Likely candidates:
  - `cargo fmt --check` finding drift in one of the 217 patched
    Cargo.toml files or the Manifest{} sweep.
  - Workspace member missing from the `--exclude` list now that
    prefix-cli exists (mirror bundle-cli's treatment).
  - wasi-sdk download URL drift.
- Re-verify under `act` before pushing.

### Effort

0.5 day. First-CI-run-after-merge regressions are usually small
mechanical fixes.

### Dependencies

None.

### Out of scope

- Adding new CI jobs (separate effort).
- act-specific edge cases (already documented in
  `scripts/ci-local.sh`).

### Status

Open. Run number `28187557158` is the latest red CI; needs
investigation.

## P1: Prefixes v1.1 migration sweep

### Background

PLAN-prefixes.md ships v1 with a deprecation-window fallback for
extensions that don't declare `preferred-prefix` +
`prefix-expansion` in their manifest. All 217 in-tree extensions
currently run on the synthetic-expansion fallback
(`sqlink-internal://<crate>`). v1.1 makes the manifest fields a
hard error.

Before v1.1 cuts over, every in-tree extension needs real
`(prefix, expansion)` pairs assigned. The substrate fork already
batch-patched the Manifest{} literals with `None, None` defaults
(wit-bindgen forces all fields) — this sweep replaces the `None`s
with sensible real values.

### Scope

- Categorize the 217 extensions by family (cli-family /
  sqlite-utils-* / dotcmd-* / vec-* / single-purpose scalars).
- Per-family prefix conventions (sketch):
  - cli-family (`bundle-cli`, `prefix-cli`, `serialize-cli`,
    etc.): prefix = `sqlink-<short>`, expansion =
    `com.tegmentum.sqlink.cli.<short>`.
  - sqlite-utils-*: prefix = `sqlite-<short>`, expansion =
    `org.sqlite.utils.<short>`.
  - Generic single-purpose (`uuid`, `json1`, `csv`, ...):
    prefix = `<name>`, expansion = `org.<community>.<name>` or
    `com.tegmentum.sqlink.ext.<name>` for in-tree experimental.
  - vec-*: prefix = `vec`, expansion = `org.faiss.vec` (or
    similar; check upstream identifiers).
- Update each extension's Cargo.toml `[package.metadata.extension]`
  block (or the equivalent — verify the actual location of these
  fields).
- Re-run encode-extension-components.sh (the wit-skew guard auto-
  rebuilds).
- Re-run extension-smoke + prefix integration tests to confirm
  no regression.

### Effort

1.5 days. Most of it is mechanical sed-style edits; a couple hours
for category decisions; rest is verification.

### Dependencies

None (the substrate already accepts both real and synthetic forms).

### Out of scope

- Globally-unique-expansion registry (v2; see PLAN-prefixes.md
  "Out of scope").
- Hard-error on missing fields — that's the v1.1 release cut, not
  this sweep. Separate commit/PR.

### Status

Not started.

## P1: Live `.prefix prefer` (bare-name re-registration on pin write)

### Background

PLAN-prefixes.md's `.prefix prefer NAME EXTENSION` writes a row to
`__sqlink_prefix_pin` but the bare-name re-registration only fires
on next session. v1 simplification documented in the dot-cmd help
text. Operators who want the pin to take effect today have to
restart the cli.

### Scope

- Inside `.prefix prefer`'s implementation, after writing the
  `__sqlink_prefix_pin` row:
  - Find every other extension that has registered
    `(function_name, n_args)` at the same arity.
  - For each, look up the FUNCTION pointer / wasm dispatch ID
    via the substrate's `Host` registration cache.
  - Re-register the bare name with SQLite pointing at the pinned
    extension's implementation (use the same `register_host_loaded_*`
    path the register-X impls use).
- `prefix unprefer NAME` similarly re-registers per SQLite default
  (last-loaded wins) — easiest via re-applying the load order to
  the bare name.
- Add an integration test (`prefix_prefer_live_swap`) that verifies
  the bare-name dispatch changes within a single session after
  `.prefix prefer`.

### Effort

0.5 day. Mostly plumbing through existing structures.

### Dependencies

None.

### Out of scope

- Cross-database pin synchronization (v2).
- Pin lock-in (preventing rename once functions registered; v2).

### Status

Not started.

## P1: Cosmetic warnings cleanup

### Background

Two pre-existing warnings every fork mentions:

- `non_snake_case` in `sqlink-host` bin (probably the binary's
  entry-point fn or a generated identifier).
- Unused import at `cli/src/lib.rs:771`
  (`bindings::sqlite::extension::types::SqlValue`).

Cosmetic only; not load-bearing. Worth cleaning so future forks
don't keep flagging them.

### Scope

- Fix the `non_snake_case` site (probably `#[allow(non_snake_case)]`
  with rationale or rename if the binding name doesn't matter).
- Remove the unused import OR add `#[cfg(...)]` if it's
  conditionally used.
- Run `cargo clippy --workspace` to catch any other low-hanging
  warnings while in the file.

### Effort

15 minutes — two 1-line fixes.

### Dependencies

None.

### Out of scope

- Workspace-wide clippy cleanup (separate effort).
- Auto-format pass.

### Status

Not started.

## P2: Single-slot hook audit-only recording

### Background

PLAN-prefixes.md's substrate intentionally skipped single-slot
hooks (authorizer / update / commit / wal) — they have no name to
qualify and at most one per connection, so the prefix model
doesn't apply. v1.1 can add audit-only recording into
`__sqlink_prefix_function` for diagnostic visibility.

### Scope

- In each of the four `register_X_hook` host impls (authorizer,
  update, commit, wal), record a synthetic row in
  `__sqlink_prefix_function`:
  - `function_name` = `__hook_<shape>` (e.g. `__hook_authorizer`)
  - `n_args` = sentinel value like `-1`
  - `extension_name` = the registering extension
- `.prefix conflicts` already SELECTs by `function_name` — these
  synthetic hook rows would surface in the conflicts view if
  multiple extensions register the same hook (last-wins by SQLite
  default, but the operator can now see who's claimed the slot).
- Add an integration test exercising this for at least one hook
  shape.

### Effort

0.5 day.

### Dependencies

None.

### Out of scope

- Actually qualifying single-slot hooks (they have no name; not
  meaningful).
- Operator-resolvable hook collisions (would require deeper
  changes to SQLite's hook API; SQLite enforces last-wins
  natively).

### Status

Not started.

## P2: `install_loaded_extension` refactor (DRY)

### Background

Recovery fork (commit `d6256f5`) extracted
`Host::ensure_prefix_for_extension` + `Host::record_function_for_extension`
helpers and wired them into the 4 `register_X` impls. But
`install_loaded_extension` (host/src/lib.rs:7197) still has its
own inline prefix-recording logic — left as-is because it has
"extra collision-warn + pin-lookup logic that doesn't shape
cleanly into the new helpers" (per the recovery fork's report).

Now that the helpers are stable + the integration tests are
passing, refactor `install_loaded_extension` to call them too.
Eliminates ~50 lines of duplicate logic.

### Scope

- Read `install_loaded_extension` carefully to identify the
  collision-warn + pin-lookup logic that didn't fit the helpers.
- Either:
  - Extend `record_function_for_extension` to return enough info
    that `install_loaded_extension` can do its collision-warn
    inline, OR
  - Add a third helper `Host::warn_on_function_collision(...)`
    that both code paths can use.
- Replace install_loaded_extension's inline logic with helper
  calls.
- Verify: `cargo test -p sqlink-host --lib prefix_registry::tests`
  + `cargo test -p extension-smoke --test extension_smoke_prefixes`
  both still pass.

### Effort

0.5 day.

### Dependencies

Depends on the existing helpers landing (already on main).

### Out of scope

- Renaming install_loaded_extension (it's pub; sqlink-native
  imports it).

### Status

Not started.

## P2: Bundles browser composed-cli `dispatch_dot_command` driver

### Background

PLAN-bundles.md v1 noted: the browser composed-cli has no
`dispatch_dot_command` driver — `extension-loader.js:12` returns
404 when bundle-cli's `.bundle save` etc. dispatch through it.
v1.1 polish fork added the bundle-cli to browser PICK list but
the actual end-to-end browser test (`composed-bundle.spec.js`)
uses `test.skip()` with a documented reason.

Same gating applies to `.prefix` in the browser if/when prefixes
land cleanly there.

### Scope

- Build a dispatch driver in the composed cli's JS layer that
  routes `.xxx CMD ARGS` calls to the loaded extension's
  `dispatch_dot_command` export.
- Verify browser composed-cli round-trips a `.bundle save myset
  --no-build` through the dispatcher.
- Update `composed-bundle.spec.js` to remove the `test.skip()` and
  exercise the full round-trip.
- Mirror in `composed-prefix.spec.js`.

### Effort

1-2 days. Browser-side wasi-cli plumbing has its own learning
curve.

### Dependencies

None (the wasm side is already in place).

### Out of scope

- Streaming output for long-running dot-cmds (v2).
- Browser-side `.bundle build` (intentionally errors out per the
  existing stub — wasm components can't spawn processes).

### Status

DONE. The original framing ("add a dispatch_dot_command driver in
extension-loader.js") was a red herring. Investigation surfaced
that bundle-cli + prefix-cli are EMBEDDED in the composed cli
component via `include_bytes!` in `cli/src/lib.rs` and dispatch
INTERNAL to the wasm cli — the JS host-import path was never the
right surface. The actual gap was a public method on
`ComposedDatabase` (browser/src/sqlink-composed.js) to drive a
single dot-command through the existing sentinel-bounded stdin
pipe.

Resolved by `ComposedDatabase.execDotCommand(line)` (commit
`aca8d484`), the sibling of `exec()` that writes a raw dot-cmd line
+ sentinel SELECT through the persistent stdin queue and returns
the cli's stdout window. `composed-bundle.spec.js` and
`composed-prefix.spec.js` now exercise `.bundle save / list / show
/ delete` and `.prefix add / list / expansion / delete`
end-to-end against the composed cli with no skip — assertions
substring-match the cli's actual stdout.

## P2: Bundle multi-name aliasing

### Background

PLAN-bundles.md v1 supports a single name per `set_hash` via the
`__cas_bundle.name UNIQUE` constraint. True multi-name aliasing
needs a separate `__cas_bundle_alias` table so two names can
point at the same set_hash.

### Scope

- New `__cas_bundle_alias` table:
  ```sql
  CREATE TABLE __cas_bundle_alias (
      name TEXT PRIMARY KEY,
      bundle_id INTEGER NOT NULL REFERENCES __cas_bundle(id) ON DELETE CASCADE,
      created_at INTEGER NOT NULL
  );
  ```
- Migrate `__cas_bundle.name` from PRIMARY KEY to nullable
  display-name; aliases move to the new table.
- Update `bundle_save` + `bundle_find_by_name` to use the alias
  table.
- Update `.bundle list` to show all aliases per row.
- Test: two distinct names pointing at the same set_hash.

### Effort

1 day (schema migration + bundle-cli + cas-cache API + tests).

### Dependencies

None.

### Out of scope

- Cross-database alias sync (v2).

### Status

Documented v1.1 in PLAN-bundles.md.

## P2: `.load` auto-cache into cas-cache by content-hash

### Background

Currently `.load /path/to/foo.component.wasm` doesn't push the
bytes into cas-cache by content-hash; it just opens the file
directly. After `.load`, `--bundle-load` referencing that
extension hits the cas-cache-miss error path because the
extension's bytes are only on the operator's filesystem, not in
the registry.

### Scope

- After a successful `.load PATH`, compute content-hash + insert
  into cas-cache via the existing `cas_cache.put_bytes` API.
- Update `--bundle-load` resolution to check both cas-cache and
  the original PATH (if recorded somewhere).
- Test: load → save bundle → restart cli → `--bundle-load` works
  without manual cas-cache priming.

### Effort

0.5 day.

### Dependencies

None.

### Out of scope

- Auto-pruning loaded-but-not-bundled extensions from cas-cache.

### Status

Documented v1.1 in PLAN-bundles.md.

## P2: HTTPS resolver mocking infrastructure

### Background

cargo-mutants triage flagged 3 surviving mutants in
`sqlite-cas-cache/src/resolver.rs:179, 187` (`HttpsResolver::resolve`,
delete `!`) because the tests don't have an HTTPS mock — they
either skip the network path entirely or run against the real
internet (flaky in CI).

### Scope

- Add `mockito` (or `wiremock-rs`) as a dev-dependency.
- Spin up a mock HTTPS server in the test fixtures that returns
  controllable responses.
- Replace the network-path tests with mocked variants.
- Mutation re-run on resolver.rs to confirm the 3 mutants now die.

### Effort

0.5 day.

### Dependencies

None.

### Out of scope

- Replacing the production HTTPS client (still uses reqwest).

### Status

Not started.

## P2: Hostile-filesystem test fixtures

### Background

cargo-mutants flagged `store::open_external` (delete `!` on a
file-existence check) as surviving because no test exercises the
error path with EACCES / read-only parent dir. Possible with a
`tempfile::TempDir` + `std::os::unix::fs::PermissionsExt`, but
flaky in CI runners with permission-stripping (containers
sometimes ignore chmod).

### Scope

- Add `permissions-ci-skip` helper that conditionally skips when
  the runner can't enforce filesystem permissions.
- Test: create a read-only parent dir, attempt `open_external`,
  assert it fails gracefully.
- Mutation re-run to confirm mutant dies.

### Effort

0.5 day.

### Dependencies

None.

### Out of scope

- Adversarial-filesystem testing beyond permissions (NFS races
  etc.).

### Status

Not started.

## P2: Schema-migration test fixtures

### Background

cargo-mutants flagged `store::install_schema:274, 279` (the
v1→v2 and v2→v3 migration arms) as surviving — no test writes a
legacy schema-version row + opens to trigger the migration.

### Scope

- Add a `legacy_v1_db_fixture()` helper that creates a fresh
  cas-cache db with `__cas_meta.schema_version = '1'` + the v1
  schema only.
- Test: open with current store, assert migration runs +
  schema_version updates + new tables exist.
- Same for v2.
- Mutation re-run to confirm both arms now die.

### Effort

0.5 day.

### Dependencies

None.

### Out of scope

- Backward-migration (downgrade) tests — not supported.

### Status

Not started.

## P2: cargo-mutants re-run

### Background

The mutation-testing round (commits 9ca5128, 7267a13, a0c95dd)
added 6 new tests closing 8-10 mutants in sqlite-cas-cache.
Estimated post-fix kill rate: **92-94%** — but never re-measured.
Worth running again after the architectural-mutant work above to
confirm + get a fresh number.

### Scope

- `cargo mutants -p sqlite-cas-cache --output mutants-out-cas-cache
  --timeout-multiplier 3 --in-place` — ~22 min runtime.
- `cargo mutants -p sqlink-loader` — ~6 min runtime.
- Report the actual caught/missed counts.
- Close any new mutants that surfaced from the architectural
  work.

### Effort

0.5 day total (most is wall-clock for the runs).

### Dependencies

The HTTPS-mocking + hostile-fs + migration-arm work above (so the
re-run sees the improvement).

### Out of scope

- Adding mutation testing to additional crates (sqlink-host
  baseline scan is ~80-150 mutants per the prior research; the
  initial scope was intentionally narrow).

### Status

Not started.

## P3: More fuzz targets

### Background

The fuzz infra round added 5 targets:
- `policy_check_manifest`
- `cas_put_bytes_roundtrip`
- `bundle_save_set_hash`
- `parse_duration`
- `parse_load_args`

Three more were considered but not implemented:
- `spawn_build` path validation (post-P0 security fixes — verify
  the validators reject the path-escape attacks they should).
- `bundle-cli` argv parser (untrusted operator input).
- `sqlink-cli-argv` parser (untrusted argv, already extracted as
  a native crate).

### Scope

- One fuzz_targets/*.rs per target.
- Seed corpus from prior P0 attack patterns + existing argv
  fixtures.
- 5-min smoke runs in fuzz-smoke CI per target.

### Effort

1 day (3 small targets).

### Dependencies

None.

### Out of scope

- Differential fuzzing across parsers (v2).

### Status

Not started.

## P3: Longer fuzz runs

### Background

The extended-fuzz round (`test/fuzz-cas-extended` branch, merged)
ran `cas_put_bytes_roundtrip` + `bundle_save_set_hash` for 20 min
each with no crashes. Coverage plateaued at ~417/549 feature
edges. Sharply diminishing returns at this point — going to
hours per target would explore deeper paths but with no
guarantee of finding anything.

### Scope

- One-shot extended run (4-6 hours per target, weekend wall-clock).
- Report coverage delta vs the 20-min runs.
- Triage any findings.

### Effort

0.5 day human + ~12 hours wall-clock per target.

### Dependencies

None.

### Out of scope

- Continuous fuzzing infra (OSS-Fuzz integration — separate plan).

### Status

Not started.

## Fan-out strategy

Group items into rounds based on dependency + region:

### Round 1 (sequential or single fork)

- P0 CI green-up — must precede anything else; pushes can't be
  trusted while CI is red.

### Round 2 (parallel forks, no overlap)

- Fork A: P1 cosmetic warnings (15 min) + P1 live `.prefix prefer`
  (0.5 day) — both small + low-risk.
- Fork B: P1 prefix v1.1 migration sweep (1.5 days) —
  217-extension Cargo.toml batch edit.
- Fork C: P2 install_loaded_extension refactor (0.5 day) — DRY-up.
- Fork D: P2 single-slot hook audit recording (0.5 day) —
  audit-only path.

Round 2 finishes ~1-2 wall-clock days assuming parallelism.

### Round 3 (parallel forks)

- Fork E: P2 bundle multi-name aliasing (1 day) + .load auto-cache
  (0.5 day) — both touch cas-cache + bundle-cli.
- Fork F: P2 bundle browser dispatch driver (1-2 days) —
  composed-cli JS layer.
- Fork G: P2 HTTPS resolver mocking + hostile-fs + migration-arm
  test fixtures (1.5 days total) — test-infra trio.

### Round 4 (sequential)

- P2 cargo-mutants re-run (depends on Round 3's Fork G).
- P3 more fuzz targets.
- P3 longer fuzz runs.

## Estimated total effort

| Round | Items | Sequential effort | Parallel wall-clock |
|---|---|---|---|
| Round 1 | P0 CI | 0.5 day | 0.5 day |
| Round 2 | 4 P1 items | 3 days | 1-2 days |
| Round 3 | 4 P2 items | 4-5 days | 1-2 days |
| Round 4 | 3 mixed items | 1.5 days + wall-clock | 1 day + wall-clock |
| **Total** | **~12 items** | **~9-10 days sequential** | **~3-5 days parallel** |

Parallel-fork pacing assumes the established commit-per-step
discipline + worktree-per-fork pattern.

## Out of scope (genuinely deferred)

These items appear in various PLAN-*.md "Out of scope" sections
and are listed here only for visibility — no work planned in this
roadmap:

- **Cross-target builds** (bundles): wasi-sdk / zig-cc / cross.
  Per-user setup; not per-bundle.
- **Bundle registry/publishing**: tegmentum-org bundle registry
  for sharing across machines.
- **Bundle `--with-schema` / `--with-data`**: overlaps with
  wal-archive territory.
- **sqlink-host as rlib**: production-install path so generated
  crates depend on rlib not workspace source.
- **Per-query prefix overrides** (prefixes): `SELECT prefix
  foaf=other; foaf__name(...)` SQL syntax.
- **Prefix-scoped permissions**: gate per-prefix at capability
  layer ("only operator can use system__*").
- **Prefix registry hosting**: org-wide registry verifying
  globally-unique expansions.
- **Cross-database prefix sync**.
- **Bulk prefix import/export**.
- **OSS-Fuzz integration**: continuous fuzzing infra (separate
  CI investment).
- **Workspace-wide clippy cleanup**: cosmetic but large.

## References

- `PLAN-bundles.md`, `PLAN-prefixes.md`, `PLAN-cas-cache.md`,
  `PLAN-wal-archive.md` — source plans for items derived from
  v1 deferrals.
- Recent commits referenced inline (e.g. `d6256f5` for the
  hot-path helpers, `9ca5128`/`7267a13`/`a0c95dd` for the
  mutation test additions).
