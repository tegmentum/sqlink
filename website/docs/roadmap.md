---
sidebar_position: 3
title: Roadmap
description: What's coming next  v1 follow-ups grouped by priority.
---

# Roadmap

The v1 ship landed bundles + prefixes + cas-cache + cli + tests + CI
+ fuzz/mutation infra. Several smaller items surfaced during
implementation got tagged "v1.1" or "deferred." This page summarises
those; the full priority + sequencing + effort breakdown lives in
[PLAN-followups](/plans/PLAN-followups).

## P0 — blocking

- **CI green-up.** Several recent pushes went red on real GitHub
  Actions despite local act-based verification (cargo fmt drift,
  tokio-on-wasm, sibling-repo path-deps for compression-multiplexer
  + tvm-wasm). All sequentially fixed; tracking the latest run.

## P1 — high value, parallel-able

- **Prefix v1.1 migration sweep.** All 217 in-tree extensions
  currently run on the synthetic-expansion fallback. Before v1.1
  makes the manifest fields a hard error, every extension needs real
  `(preferred-prefix, prefix-expansion)` assigned. ~1.5 days
  mechanical sweep.
- **Live `.prefix prefer`.** Pin row writes today but bare-name
  re-registration only fires next session. Live application would
  walk the existing `register_X` cache + re-register. ~0.5 day.
- **Cosmetic warnings.** Two pre-existing 1-line fixes:
  `non_snake_case` in sqlink-host bin + unused import at
  `cli/src/lib.rs:771`. ~15 minutes.

## P2 — medium

- **Single-slot hook audit recording.** Authorizer / update / commit
  / wal hooks aren't recorded in `__sqlink_prefix_function`
  (intentional v1 call). v1.1 can add diagnostic-only recording.
- **`install_loaded_extension` refactor.** The recovery fork's
  `Host::ensure_prefix_for_extension` + `record_function_for_extension`
  helpers landed but `install_loaded_extension` still has its own
  inline logic. DRY-up.
- **Bundles browser dispatch driver.** Browser composed-cli has no
  `dispatch_dot_command` driver in v1; `composed-bundle.spec.js`
  uses `test.skip()`. ~1-2 days.
- **Bundle multi-name aliasing.** v1's `__cas_bundle.name UNIQUE`
  constraint blocks true aliasing. v1.1 adds `__cas_bundle_alias`.
- **`.load` auto-cache by content-hash.** So `--bundle-load` after
  a `.load` doesn't hit the cache-miss path.
- **HTTPS resolver mocking.** cargo-mutants flagged 3 surviving
  mutants in `resolver.rs::HttpsResolver::resolve`; tests need
  mockito/wiremock to close.
- **Hostile-fs + migration-arm fixtures.** Two more architectural-
  mutant gaps the v1 mutation round flagged.
- **cargo-mutants re-run.** Confirm the estimated 92-94% kill rate
  after the new test fixtures land.

## P3 — nice-to-have

- More fuzz targets (spawn-build path validation, bundle-cli argv,
  sqlink-cli-argv).
- Longer fuzz runs (hours per target) — sharply diminishing returns
  past the 20-min mark observed in the extended round.

## Genuinely deferred to v2

These are explicitly out of any near-term roadmap and listed only
for visibility:

- Cross-target builds (wasi-sdk / zig-cc / cross).
- Bundle registry / publishing.
- Per-query prefix overrides.
- Prefix-scoped permissions (gate `system__*` to operator-only).
- Prefix registry hosting (org-wide expansion uniqueness).
- Cross-database prefix sync.
- sqlink-host as rlib for production install.
- OSS-Fuzz integration.

See the [full plan](/plans/PLAN-followups) for effort + dependencies
+ fan-out strategy.
