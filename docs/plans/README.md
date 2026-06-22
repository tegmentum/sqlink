# Plans

Working plans for sqlink. The convention is simple:

- **Top level (`docs/plans/*.md`)** holds the plans that are still
  being worked on or that capture deliberately-deferred design
  rationale awaiting a consumer. Anyone joining the project can
  look at this directory and know what's actually being worked
  on.
- **`docs/plans/archive/`** holds plans for work that has fully
  shipped or has been superseded. Most archived plans carry a
  status header documenting the shipping commits.

Each plan starts with a `Status` block (usually a blockquote near
the top) that records the current state — what phases have shipped,
what's outstanding, and any reference commit shas.

## What's active right now

| Plan | Theme |
|---|---|
| `PLAN-benchmarks.md` | Living benchmark record (re-run after perf changes) |
| `PLAN-browser-runtime.md` | Run sqlite-cli in browser via wasi-polyfill |
| `PLAN-interactive-capture.md` | Design notes for interactive changeset capture (deferred until a consumer asks) |
| `PLAN-release-readiness-code-health.md` | OSS-release tech-debt audit + punch list |
| `PLAN-release-readiness-perf-ux.md` | OSS-release perf + UX punch list |
| `PLAN-release-readiness-testing.md` | OSS-release testing audit + punch list |
| `PLAN-speculative.md` | Items deliberately unstarted until a concrete consumer materializes |
| `PLAN-tvm-integration.md` | TVM track shipped; Phase 3 (wasm64) pending upstream toolchain |
| `PLAN-wasmmachine.md` | wasmMachine integration scaffolding shipped; v86-tool-dependent steps remain |

## Authoring conventions

- One file per plan, named `PLAN-<topic>.md`.
- Lead with goal + status. Phases or stages numbered so commits
  can reference them (`feat(foo): PLAN-foo.md Phase 2`).
- Update the status header when a phase ships; cite the commit.
- When the plan completes, move it to `archive/` rather than
  deleting it — historical context helps future readers.
- New plans created via `tooling/plan-add.py`.
