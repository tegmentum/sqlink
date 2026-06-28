# PLAN: Migrate sqlink-shim-codegen into datalink (α refactor)

## Status (2026-06-28)

User has locked **option (α)** from #561: move the shim-bridge codegen
into the shared `~/git/datalink` substrate, parallel to how
`shim_sqlite!` / `shim_duckdb!` already split the extension-core macros
in `datalink-extcore`. This plan captures the structural analysis +
migration sequence so it can be executed in a future session.

## Problem

`sqlink-shim-codegen` is sqlink-private today and emits SQLite-shaped
wasm bridges (`sqlite:extension@1.0.0`). When the framework extends to
DuckDB (via ducklink) — and longer-term to datafission — we need the
codegen to support per-database emit without duplicating the substrate.

The proof point for the split already exists in `datalink-extcore`:
- `shim_sqlite!` (193 LoC) — generates sqlink extension shim
- `shim_duckdb!` (301 LoC) — generates ducklink extension shim
- ~50% of the macros is shared structurally (marshalling pattern,
  DECLS dispatch loop, NULL handling); the LoC delta is genuinely
  per-database hooks (handle table, lifecycle, registration, typed
  errors).

The shim-bridge codegen is a code-generator (~25K LoC per output) not
a macro (~200-300 LoC per output), but the same core/emit split fits.

## Target shape

```
~/git/datalink/
  crates/
    datalink-shim-codegen-core/        # NEW — Tier 2 substrate
      src/
        interface_db.rs                # SQLite parsing from /tmp/*-interface.sqlite
        wit_parse.rs                   # walker + reachability + record registry
        record_registry.rs             # canonical-CBOR type-id derivation
        force_link.rs                  # reachability-filtered __FORCE_LINK_*
        compose_emit.rs                # compose.wac auto-emission (#563)
        override_tables.rs             # operator/passthrough/tuple_pick overrides
        name_match.rs                  # #490 walker + W1 prefix-strip
    datalink-shim-sqlite-emit/         # NEW — sqlink target
      src/
        dispatch.rs                    # SqlValue marshalling + ParamShape/RetShape
        emit_lib.rs                    # sqlite:extension dispatch arms
        emit_wit.rs                    # bridge WIT world + serde-ops local records
        emit_cargo.rs                  # bridge crate Cargo.toml
        emit_readme.rs                 # consume has_compose_wac (#563)
        vtab.rs                        # #531 CREATE TABLE schemas + #532 row mat
    datalink-shim-duckdb-emit/         # NEW — ducklink target (future)
      src/
        dispatch.rs                    # Duckvalue marshalling
        emit_lib.rs                    # duckdb:extension callbacks (6 arms)
        emit_wit.rs                    # bridge WIT for duckdb world
        emit_cargo.rs                  # Cargo.toml
        emit_readme.rs
        table_func.rs                  # Resultset returns (vs vtab cursor)
        register.rs                    # explicit register_scalars()
        lifecycle.rs                   # load/reconfigure/shutdown
  tooling/
    shim-codegen/                      # CLI that dispatches per-target
      src/main.rs                      # --target sqlite | duckdb
```

`sqlink-shim-codegen` becomes a thin CLI wrapper that:
1. Adds `datalink-shim-codegen-core` + `datalink-shim-sqlite-emit` as
   workspace deps
2. Reduces to ~100 LoC of CLI glue
3. Optionally retires when datalink's `tooling/shim-codegen/` ships

## What's actually shared vs per-database

Quantitative breakdown of current sqlink-shim-codegen (post-#565
estimate, ~12-15K LoC):

| Category | LoC est. | Bucket |
|---|---:|---|
| Interface DB parsing | ~1,500 | core |
| WIT walker + reachability (#565) + record registry | ~3,500 | core |
| Force-link emission (#557fix + #565 filter) | ~400 | core |
| compose.wac emission (#563) | ~500 | core |
| Override tables (operator/passthrough/tuple_pick) | ~250 | core |
| Name-matching heuristic (#490 + W1) | ~800 | core |
| Reachability + RecordType + alias resolution | ~1,500 | core |
| **Core subtotal** | **~8,450** | **65-70%** |
| Dispatch arm emission (SqlValue match arms) | ~2,500 | sqlite-emit |
| Vtab CREATE TABLE schemas + multi-col xColumn | ~1,000 | sqlite-emit |
| Bridge WIT + serde-ops local records | ~800 | sqlite-emit |
| Cargo / README / misc | ~400 | sqlite-emit |
| **SQLite emit subtotal** | **~4,700** | **30-35%** |

Projection for duckdb-emit (when added): ~5-7K LoC due to:
- Handle table machinery (analog of shim_duckdb's ~30 LoC per
  extension, scaled to per-function dispatch)
- `Resultset` table functions (vs SQLite's vtab cursor model)
- `call_scalar_batch` + per-row loop
- `register_scalars()` with explicit `Logicaltype` translation
- `load`/`reconfigure`/`shutdown` lifecycle
- Typed `Duckerror` instead of String

Total estimated framework after migration:
- core: ~8.5K LoC (shared across all DBs)
- sqlite-emit: ~4.7K LoC
- duckdb-emit: ~5.5K LoC (when added)
- Compared to current: a small net growth for the duckdb-emit, but
  ZERO redundancy — DuckDB-only work lives in DuckDB-only files.

## Key architectural decisions (locked by this plan)

### D1. NeutralValue extension for record types

The `NeutralValue::Complex { type_expr, json }` escape hatch already
exists in `datalink-extcore` (FROZEN closed set + this one arm). Shim
bridges ferry **geometry / raster / topology / tfloat-sequence / etc.**

**Decision: ride the existing Complex escape.** `type_expr` becomes
the symbolic name (e.g., `"postgis:wasm/types@0.1.0/geometry"`),
`json` becomes the canonical-CBOR bytes (matches our wit-value magic-
prefix convention from #559). Per-database adapters lift Blob ↔
Complex at the value-marshalling boundary using their host's typed-
value registry. **No NeutralValue extension required.**

This means the FROZEN set stays FROZEN; the per-database adapters
extend the escape hatch's interpretation, not the value model.

### D2. Per-database UDTF model

SQLite's vtab + xColumn vs DuckDB's `Resultset` returns are
architecturally different. Solutions:

- The core extracts the SHAPE of each UDTF (param types, row shape,
  HIDDEN args) into a database-agnostic IR.
- Per-database emit produces:
  - SQLite: vtab module + CREATE TABLE + xColumn dispatch (today's
    #531 + #532 work)
  - DuckDB: callback that builds + returns `Resultset`

Both emit-sides consume the same IR; the IR carries the row shape +
HIDDEN/visible column metadata. Neither needs to know the other.

### D3. Registration model

SQLite scalars are described in `Manifest::scalar_functions`; the host
wires them at load time. DuckDB requires the extension to actively
call `registry.register(name, args, ret, callback, opts)` against
the runtime's scalar capability.

**Decision: per-database emit owns registration.** Core produces the
function metadata (name, args, ret, deterministic, etc.) into an IR;
sqlite-emit puts it in Manifest; duckdb-emit emits `register_scalars()`.

### D4. Magic-prefix wit-value lift survives the migration

The 36-byte header `WTV\x01 + type_id + canonical-CBOR` (#559) is
host-side convention used by sqlink-loader. It's per-loader, not per-
database. ducklink-loader (or its equivalent) needs the same lift on
its side. Both inherit the convention from `datalink-loader-core`
when that crate exists (Tier 2 / `datalink-runtime` per CONSOLIDATION.md).

## Migration sequence

Four steps, each independently mergeable:

### Step 1 — In-tree refactor of sqlink-shim-codegen

Reorganize modules WITHOUT moving to datalink yet:

```
sqlink-shim-codegen/src/
  core/                   # what will become datalink-shim-codegen-core
    interface_db.rs
    wit_parse.rs
    record_registry.rs
    force_link.rs
    compose_emit.rs
    override_tables.rs
    name_match.rs
  emit_sqlite/            # what will become datalink-shim-sqlite-emit
    dispatch.rs
    emit_lib.rs
    emit_wit.rs
    emit_cargo.rs
    emit_readme.rs
    vtab.rs
  main.rs                 # CLI; --target sqlite (default)
```

Verification: no behavior change. Regen postgis + mobilitydb bridges
byte-identical to current main.

### Step 2 — Move `core/` to datalink

Extract `core/` modules into new `datalink/crates/datalink-shim-codegen-core/`
crate. `sqlink-shim-codegen` adds the git dep and re-exports.

Verification: bridges still regen byte-identical.

### Step 3 — Move `emit_sqlite/` to datalink

Extract `emit_sqlite/` modules into
`datalink/crates/datalink-shim-sqlite-emit/`. `sqlink-shim-codegen`
becomes a ~100 LoC CLI wrapper around the two datalink crates.

Verification: bridges still regen byte-identical.

### Step 4 — Add datalink-shim-duckdb-emit (when ducklink is ready)

New crate in datalink. Follows the IR established in steps 1-3. CLI
gets `--target duckdb` arm. ducklink-shim-codegen could become a thin
sibling wrapper.

Verification: regenerate the same postgis-interface.sqlite against
`--target duckdb`; bridge component builds against DuckDB's WIT
contract; loads in ducklink-loader; at least one PostGIS scalar
round-trips.

## Sequencing with other in-flight work

- **#552 W3.6 hold** — independent of migration; resolve or close
  when surfaced.
- **#561 cross-project framework support** — folds into this plan;
  steps 1-4 ARE its execution.
- **#565 / #563 / #557fix family** — already merged on current main;
  migration step 1 picks them up as-is.
- **datalink-runtime (Tier 2 in CONSOLIDATION.md)** — orthogonal but
  related. The shim-bridge codegen migration doesn't gate on it; both
  can land independently.
- **Wasmtime version skew** — datalink-runtime gates on sqlink/ducklink
  matching wasmtime versions. Shim-codegen migration doesn't care.

## Risks

- **Behavior drift during refactor.** Each step needs a regen-and-diff
  check against the prior step's output. Mitigate: golden-file tests
  in the codegen.
- **Per-database IR design churn.** The core/emit IR will probably need
  iteration. Mitigate: write IR with only SQLite emit consuming it
  first (steps 1-3); revisit when DuckDB emit lands.
- **Datalink workspace structure already chose `valuemodel` + `extcore`
  as separate crates.** A `shim-codegen-core` should match that style.
- **Wasm-component target idiosyncrasies (force-link, compose.wac,
  reachability) ARE substrate.** They belong in core, not per-DB. Don't
  accidentally classify them as per-DB.

## Open decisions (capture, don't lock)

- **DD1. CLI surface.** sqlink-shim-codegen retires, or stays as a thin
  wrapper for backward-compat scripts that already call it?
- **DD2. Datalink workspace inclusion.** Add as Tier-2 crates alongside
  `datalink-runtime` per CONSOLIDATION.md, or as a separate Tier-1
  tooling-style addition?
- **DD3. IR format between core and emit.** Plain Rust structs in a
  shared module, OR something more structured (CBOR-serializable for
  potential build-time caching)?
- **DD4. Test infrastructure.** Per-emit golden files, OR a shared
  contract test in the core that each emit validates against?

## Verification

- Step 1: regen postgis + mobilitydb bridges byte-identical.
- Step 2: same, after core extraction.
- Step 3: same, after emit extraction.
- Step 4: postgis bridge regenerated for DuckDB target builds + loads
  in ducklink-loader; at least one scalar (`ST_AsText` is a good
  candidate — minimal dependencies) round-trips.

## References

- `~/git/datalink/CONSOLIDATION.md` — the tiered consolidation strategy.
- `~/git/datalink/crates/datalink-extcore/src/shim_sqlite.rs` — the
  shim_sqlite! macro (193 LoC; precedent for shared substrate).
- `~/git/datalink/crates/datalink-extcore/src/shim_duckdb.rs` — the
  shim_duckdb! macro (301 LoC; precedent for per-DB delta).
- `~/git/sqlink/docs/plans/PLAN-shim-tooling-residue.md` — captures the
  current state of the codegen (W1-W5, #557fix, #563, #565, etc.).
- `~/git/sqlink/sqlink-loader/` — sqlite:extension loader, the SQLite
  pole of the per-host adapter.
