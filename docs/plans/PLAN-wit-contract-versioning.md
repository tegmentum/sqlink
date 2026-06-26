# PLAN: WIT contract versioning + load-time compatibility guard

## Problem

sqlink's value lives in a WIT contract (`sqlink:wasm` — `sqlite:extension`'s
`value-type`/`sql-value`, the low-level/high-level/dispatch/callbacks
interfaces) shared between the host and ~239 wasm component extensions. When that
contract changes, a component built against the **old** shape can be loaded into a
host speaking the **new** shape. If the change altered the canonical ABI (e.g. a
new `value-type` variant shifting enum discriminants, or a record field added),
the component instantiates but **marshals corrupted values** — wrong data, no
error. This is the failure mode the sibling ducklink project hit head-on (its WIT
was entirely unversioned, so its rich-types bump silently shifted
`logicaltype`/`duckvalue` discriminants). We want a guard that turns that silent
corruption into a clean, early, actionable rejection.

## Where sqlink stands today (ahead of ducklink, with gaps)

What's already right:
- **The WIT packages are versioned**: `package sqlink:wasm@0.1.0;` for the shared
  contract, `package sqlite:ext-<name>@0.1.0;` per component. So wasmtime's
  instantiation already does semver-compatible matching: a component importing
  `sqlink:wasm/...@0.1.x` is satisfied by a host providing `@0.1.y` (y≥x) but
  **rejected** by a different MAJOR. The hard guard partly exists for free —
  ducklink had nothing.
- **A Manifest channel exists**: components export `get-info` →
  `extension-info { name, version, functions }`. The host already reads it to
  discover functions, so it's a natural place to also carry a contract version
  (model-level, readable before any dispatch). ducklink's imperative `load()`
  model has no equivalent.
- **The contract is intrinsically stable**: `value-type` is SQLite's five fixed
  storage classes (integer/float/text/blob/null). SQLite is dynamically typed, so
  unlike DuckDB's logical-type set (which grew 6→21), this enum essentially never
  changes. Breaking bumps to the core value contract should be **rare** — this is
  hygiene + future-proofing, not firefighting active churn.

The gaps (the same (b)-layer ducklink is closing):
- **No discipline / policy** for when to bump the package version. Everything is
  `@0.1.0`; no contract change has ever been versioned, so the versioning isn't
  yet *load-bearing*.
- **No friendly pre-check.** The host (`host/src/lib.rs`) instantiates and lets
  wasmtime fail — a cryptic trap, not "rebuild this component for contract vN".
- **The registry records no contract version.** `registry/index.json` entries
  have `version`, `min_sqlite_version`, `checksum`, `oci_artifact`, … but nothing
  saying which contract a component targets.
- **No catalog-verify enforcement** (sqlink has no `verify-catalog` equivalent
  to catch drift before publish).
- **Some type files lack a `package` line** (`sqlite-extension.wit`,
  `sqlite-low-level.wit`, `sqlite-high-level.wit`); they must be confirmed to
  belong unambiguously to the versioned package.

## Plan

### (a) Make the versioning load-bearing — policy + baseline

1. **Confirm package membership.** Ensure the type-bearing interfaces
   (`sqlite:extension` value-type/sql-value, `low-level`, `high-level`,
   `dispatch`, `extension-callbacks`, `zip-operations`, `library`) are all
   unambiguously inside the versioned `sqlink:wasm@…` package (add the `package`
   line where missing). The package version IS the contract version.
2. **Adopt a bump policy** (document it next to the WIT):
   - **MAJOR** bump on any change to the shared canonical ABI — adding/reordering
     a `value-type` variant, changing a `sql-value` / shared record field, or a
     breaking signature change in low-level/dispatch/callbacks. Old components are
     then cleanly rejected (wasmtime + the pre-check below).
   - **MINOR** bump on additive, backward-compatible changes (a new interface, a
     new function in a new interface). Old components keep loading.
3. **Promote the baseline to `@1.0.0`.** `@0.1.0` (0.x) has *no* semver-compat
   guarantees under the component model — within 0.x, even minor differences can
   be treated as incompatible, and there's no "breaking vs additive" signal. Cut
   the current stable contract as `sqlink:wasm@1.0.0` so the major-based guard has
   real meaning going forward. (Coordinate with the per-component
   `sqlite:ext-<name>` packages — they can keep their own independent versions;
   what matters is the **imported** `sqlink:wasm` version.)

Because the contract is stable, (a) is mostly **declaration + policy** — not a
forced mass rebuild. The 239 components only need rebuilding when an actual MAJOR
bump happens (and the policy makes that an explicit, tracked event).

### (b) The guard — registry field, Manifest channel, loader pre-check, verify

1. **Registry field.** Add `wit_contract: "1.0.0"` to every `registry/index.json`
   entry, derived from the component's actual imported `sqlink:wasm` version
   (`wasm-tools component wit`). This makes the contract a first-class, queryable
   property and feeds the OCI metadata (a `wit-contract` label / tag suffix
   alongside `checksum`/`oci_artifact`).
2. **Manifest channel (sqlink-specific advantage).** Add a `contract-version: u32`
   (the MAJOR) — or a `contract: string` — field to `extension-info` (the
   `get-info` record). The host reads `get-info` *before* registering/dispatching
   the extension's functions and can reject a contract mismatch with a friendly,
   model-level message even for components that (in a composed/dynlink path) might
   otherwise slip past interface matching. This is cheap and additive.
3. **Loader pre-check** in `host/src/lib.rs` (the dynamic `.load` /
   `extension-loader` path and the compose provider). Before instantiating:
   introspect the component's imported `sqlink:wasm` package version via the
   wasmtime component-type API (`component.component_type().imports(&engine)`),
   parse the MAJOR, and compare to a single host constant
   `CONTRACT_MAJOR = 1`. On mismatch (or an unversioned legacy component), reject
   with: *"extension '<name>' targets sqlink contract <X> but this host speaks
   contract 1.x; rebuild it against the current WIT (or use the matching sqlink
   host)."* Belt-and-suspenders with wasmtime's own matching, but it yields the
   readable message and catches the Manifest-only / composed cases. Expose the
   host's own contract version (a log line at load and/or a host built-in) so it's
   observable.
4. **Catalog-verify.** Add a `tooling/verify-catalog.py` (sqlink has none today)
   — or fold into the existing catalog tooling — that enforces, per entry:
   `wit_contract` is present and equals the catalog's target MAJOR, and (when the
   artifact is present) the built component's actual imported `sqlink:wasm`
   version matches `wit_contract`. Drift is then caught at publish time, before it
   reaches an OCI registry.

### Host ↔ contract version mapping

Document the mapping so users/bundles load compatible components, e.g. sqlink host
1.x ⇒ contract 1.x. Bundles (`PLAN-bundles.md`) should record the contract
version so a bundle is self-consistent; the OCI artifact tags should encode it.

## How this differs from ducklink (why it's lower-risk here)

| | ducklink | sqlink |
|---|---|---|
| WIT versioned today | no (`duckdb:extension`) | **yes** (`sqlink:wasm@0.1.0`) |
| Shared value enum | grew 6→21 (active churn) | SQLite's fixed 5 storage classes |
| Breaking bumps expected | frequent | rare |
| Manifest channel for a 2nd check | none (imperative `load()`) | **`get-info`/`extension-info`** |
| Per-component WIT | 16-file copies × N | one `world.wit` per component |
| Forced mass rebuild now | yes (v1→v2 cutover) | **no** (additive guard; rebuild only on the next real MAJOR) |

ducklink must do the painful v1→v2 cutover (version + rebuild all 181) *now*
because its contract is already changing. sqlink can land the guard **additively**
and pay the rebuild cost only when (if) a genuine MAJOR contract change ever
arrives.

## Relationship to `compose:dynlink` shape verification (avoid two guards)

sqlink's host already runs on the `sys:compose@1.0.0` orchestration framework
(`~/git/webassembly-component-orchestration`, `compose:dynlink@0.1.0` —
`host/src/compose_provider.rs`). That framework does **canonical WIT + CBOR →
deterministic component identity and shape verification**. That overlaps directly
with the load-time guard this plan proposes (verify a component's contract matches
the host) — so **do not harden two parallel mechanisms**.

Before building the bespoke pre-check (b.3), determine what `compose:dynlink`
already gives us:
- Its canonical-WIT hash is a **content-addressed contract identity** — stronger
  than a hand-maintained `wit_contract` semver string (a shape change *is* a hash
  change; nothing to remember to bump). The registry field could record that hash
  instead of (or alongside) the semver.
- The linker's resolve/instantiate step is the natural place to enforce
  compatibility — the guard may already live there, or be a small policy on top,
  rather than new loader code.

**Action:** scope (b.3)/(b.4) as *configuring/consuming* `compose:dynlink`'s shape
verification where it already covers us, and only hand-roll the parts it doesn't
(e.g. the friendly message, the Manifest `contract-version` channel). The same
question applies to ducklink, which is currently hand-rolling its `wit_contract`
guard — keep the two projects' answer consistent.

## Rollout (phased, low-risk)

- **Phase 1 — additive, no rebuild.** Add `wit_contract` to the registry +
  `verify-catalog`; add `contract-version` to `extension-info` (rebuild only the
  components you happen to touch; the field is optional/back-compat). Promote the
  baseline package to `@1.0.0` and write the bump policy.
- **Phase 2 — the guard.** Add the loader pre-check (`CONTRACT_MAJOR = 1`) + the
  Manifest read; surface the host's contract version. Now mismatches fail clean.
- **Phase 3 — the policy in action.** The *next* breaking contract change does a
  MAJOR bump (`@2.0.0`), a one-command WIT propagation, a tracked full-catalog
  rebuild, and an OCI re-tag — exactly the discipline that prevents silent drift.

## Verification

- A current component still loads under the contract-1 host (Phase 1/2 are
  additive — no regression); the smoke suite stays green.
- A component artifact built against a deliberately-mismatched contract (or an
  unversioned legacy one) is **rejected** by the loader with the contract-mismatch
  message — not silently run. Demonstrate both outcomes.
- `verify-catalog` fails a registry entry whose `wit_contract` disagrees with its
  built artifact's imported `sqlink:wasm` version.
