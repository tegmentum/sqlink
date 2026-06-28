# PLAN: Window function dispatch substrate across 3 targets (#616)

## Status (2026-06-28)

Captured from the substrate survey for #616. Window functions are
currently STUBBED on all three datalink targets (sqlite-emit /
duckdb-emit / datafission-emit). This plan inventories the contract
surfaces on each target, the upstream WIT availability for the
postgis window family, the IR gap in `datalink-shim-codegen-core`,
and proposes the phased work to wire window function dispatch end-
to-end without churning the substrate twice.

No code lands in this batch — survey + plan only.

## Problem

Two interface DBs carry `window_functions` rows:

- `/tmp/postgis-interface.sqlite`: **4 window functions** —
  `st_clusterdbscan`, `st_clusterintersectingwin`,
  `st_clusterwithinwin`, `st_clusterkmeans` (plus 6 aliases).
- `/tmp/mobilitydb-interface.sqlite`: **0 window functions**.

These rows do NOT flow through any of the three target emit paths
today:

- **sqlite-emit** (`datalink-shim-sqlite-emit/src/emit_lib.rs`): the
  `AggregateGuest` impl stubs `value()` + `inverse()` with `stubbed(
  "aggregate-function value (window mode not wired)", ...)`. The
  `push_aggregate_entry` helper unconditionally emits
  `is_window: false` in the `AggregateFunctionSpec` manifest entry.
  Window functions never enter the manifest at all.

- **duckdb-emit** (`datalink-shim-duckdb-emit/src/emit_lib.rs`): the
  `callback_dispatch::Guest` impl wires `call_aggregate` (whole-row-
  set fold) but does NOT export `aggregate-incr-dispatch` (the v3 WIT
  family that carries `call-aggregate-window(handle, partition,
  frame)`). The codegen world template doesn't include
  `aggregate-incr-dispatch` at all.

- **datafission-emit** (`datalink-shim-datafission-emit/src/emit_lib.rs`):
  the bridge exports `window-function-registry@1.0.0`, but the
  emitted `Guest` impl is `WINDOW_STUB` —
  `list_functions()` returns `Vec::new()`, every `compute_partition`
  + `return_type` returns `UnknownFunction`.

- **core IR** (`datalink-shim-codegen-core`): no window-function IR.
  The downstream consumer `shim-bridge-codegen-core::plan::WindowFn`
  carries `canonical_name + aliases + param_signatures` (a metadata
  bag), but there is no classifier producing a per-target dispatch
  shape — analogous to the `AggregateShape` / `AccKind` substrate
  added in #607/#611.

This plan substitutes the missing substrate, with the postgis
`postgis-clustering` interface as the only concrete upstream family
in scope.

## Substrate findings (from survey 2026-06-28)

### Axis 1 — Contract dispatch shape (3 distinct surfaces)

#### SQLite (`sqlite:extension/aggregate-function` in `~/git/sqlink/sqlite-loader-wit/wit/guest.wit`)

Window functions ride the aggregate dispatch surface:

```wit
record aggregate-function-spec {
    id: u64,
    name: string,
    num-args: s32,
    func-flags: function-flags,
    /// True if this aggregate also implements window-function ops
    /// (value() + inverse()).
    is-window: bool,
}

interface aggregate-function {
    step:     func(func-id: u64, context-id: u64, args: list<sql-value>) -> result<_, string>;
    finalize: func(func-id: u64, context-id: u64) -> result<sql-value, string>;
    /// Required only for window-mode aggregates.
    value:    func(func-id: u64, context-id: u64) -> result<sql-value, string>;
    /// Required only for window-mode aggregates.
    inverse:  func(func-id: u64, context-id: u64, args: list<sql-value>) -> result<_, string>;
}
```

**Registration model**: passive — host reads `Manifest::aggregate_functions`
at load time and registers each via `sqlite3_create_window_function`
when `is_window == true` (else `sqlite3_create_function_v2`).

**Frame semantics**: handled engine-side by sqlite3. The contract
surfaces the standard streaming-aggregate state machine: step adds
a row, inverse removes a row from the frame's tail, value emits the
current frame's value without resetting state, finalize emits +
disposes.

**Per-call shape**: per-row `args` is a `list<sql-value>` (same as
scalar/aggregate). `context-id` is the per-aggregation handle.

#### DuckDB (`duckdb:extension/aggregate-incr-dispatch@3.0.0` in `~/git/ducklink/wit/duckdb-extension/aggregate-incr-dispatch.wit`)

Window functions share the aggregate REGISTRATION surface
(`runtime.aggregate-registry.register(name, args, returns, callback,
options)` — same as plain aggregates) but get a DEDICATED dispatch
function in the v3 `aggregate-incr-dispatch` world:

```wit
interface aggregate-incr-dispatch {
    // ... init / update / combine / finalize for incremental agg

    record window-frame { start: u64, end: u64 }

    /// Compute the window aggregate over `partition` rows for the
    /// frame `frame`, returning the single value for the row this
    /// frame belongs to. The component MAY cache `partition`
    /// keyed by `handle` across calls within one partition; an empty
    /// `partition` signals the component to drop any cached state.
    call-aggregate-window: func(
        handle:    u32,
        partition: rowbatch,
        frame:     window-frame,
    ) -> result<duckvalue, duckerror>;
}
```

**Frame semantics**: engine-resolved. The host has already turned
`OVER (... ROWS/RANGE BETWEEN ...)` into concrete row offsets and
calls the component once per output row with the partition rows +
explicit `[frame-start, frame-end)` half-open range.

**Registration model**: active — the bridge calls
`aggregate-registry.register(...)` in `register_scalars()`-style
init. There is NO separate `window-registry`; the aggregate-registry
handles both, and the dispatch path (`call-aggregate` vs
`call-aggregate-window`) is chosen by the engine at query plan time.

**Per-call shape**: full partition `rowbatch` (`list<list<duckvalue>>`)
crosses every frame call (potentially many crossings per partition).
The WIT comment encourages component-side partition caching by handle.

**iswindow flag in `invokeinfo`** (in `types.wit:127`) is unrelated
to custom window-function support — it's a hint passed to
`call-scalar` letting scalars know they're being invoked inside a
window evaluation. Window REGISTRATION still goes through the
aggregate-registry path.

#### Datafission (`datafission:function-plugin/window-function-registry@1.0.0` in `~/git/datafission/wit/function-plugin/world.wit`)

Window functions get a DEDICATED registry interface, parallel to
the scalar/aggregate/table registries:

```wit
record window-function-meta {
    name: string,
    aliases: list<string>,
    param-types: param-signatures,
}

interface window-function-registry {
    list-functions: func() -> list<window-function-meta>;

    return-type: func(
        name: string,
        input-types: list<logical-type>,
    ) -> result<logical-type, function-error>;

    /// Compute window values for one partition. `args-rows[i]` is
    /// the argument tuple for the i-th row of the partition (in
    /// `ORDER BY` order when specified). The returned list MUST
    /// have the same length as `args-rows`, in the same row order.
    compute-partition: func(
        name: string,
        args-rows: list<list<scalar-value>>,
    ) -> result<list<scalar-value>, function-error>;
}
```

**Frame semantics**: NOT exposed. From the WIT comment: "Window
functions in DataFission's plugin contract get a whole-partition
compute hook rather than a streaming-frame API. The host calls
`compute-partition` once per partition with the full per-row
argument tuples (in `ORDER BY` order when one is specified) and
expects one scalar back per input row. This shape fits the first
concrete consumers — spatial clustering (ST_ClusterDBSCAN-as-
window), MEOS trajectory tcount-as-window, etc. — all of which
need the full point set before emitting any label. Streaming-frame
window functions (LAG, LEAD, RANK, …) remain built-in."

**Registration model**: passive — list-functions enumerates at
extension-register time; per-call dispatch keyed by name.

**Per-call shape**: ENTIRE partition fans out in one call. Output
is a `list<scalar-value>` with one entry per input row.

### Axis 2 — Frame model divergence (3 distinct semantics)

| Target       | Dispatch                                                | Frame resolution                        | Caller iteration               |
|--------------|---------------------------------------------------------|-----------------------------------------|--------------------------------|
| sqlite       | step / inverse / value / finalize (streaming)           | engine drives, calls per-row            | per-row                        |
| duckdb       | call-aggregate-window(partition, frame) per output row  | engine resolves frame to [start,end)    | per-row (with full partition)  |
| datafission  | compute-partition(args-rows) once per partition         | NOT exposed; component sees whole set   | per-partition                  |

The 4 postgis window functions in scope (`st_cluster*`) all
fundamentally require the **whole partition** before they can emit
a label — they are NOT streaming-frame-compatible. This means:

- **datafission** is the cleanest fit: `compute-partition` matches
  the upstream interface 1:1.
- **duckdb** works by caching `partition` keyed by `handle`,
  computing the full result on first frame call, then serving each
  frame's output value from the cache. The WIT explicitly endorses
  this pattern.
- **sqlite** is the awkward fit: the streaming `step/inverse/value`
  doesn't match whole-partition compute. The pragmatic path is
  `step` buffers each row, `finalize` is unused, `value` triggers
  the upstream compute on first invocation (caching results in
  `context-id`-keyed state), subsequent `value` calls (one per row
  in the partition, since SQLite drives per-row) read precomputed
  results by position. `inverse` is a no-op (the whole-partition
  result doesn't depend on the frame range).

### Axis 3 — Output shape (per-row return)

Each upstream postgis-clustering function returns a `list<X>` where
`X` is the per-row label:

| WIT signature                                                          | Per-row return type     |
|------------------------------------------------------------------------|-------------------------|
| `st-cluster-dbscan(list<borrow<geometry>>, f64, u32) -> list<option<u32>>` | `option<u32>` (NULL-able) |
| `st-cluster-kmeans(list<borrow<geometry>>, u32) -> list<u32>`              | `u32`                   |
| `st-cluster-intersecting(list<borrow<geometry>>) -> list<geometry>`        | `geometry` (WKB blob)   |
| `st-cluster-within(list<borrow<geometry>>, f64) -> list<geometry>`         | `geometry` (WKB blob)   |

So the per-row-return arm has TWO shapes in the postgis pilot:
**scalar primitive** (option<u32> / u32) and **geometry blob**. The
plan must cover both.

## Affected items

### Interface DB inventory

Both interface DBs use the same schema:

```sql
CREATE TABLE window_functions (
    extension TEXT NOT NULL,
    name TEXT NOT NULL,
    param_types_json TEXT NOT NULL,
    PRIMARY KEY (extension, name)
);
CREATE TABLE window_function_aliases (
    extension TEXT NOT NULL,
    canonical TEXT NOT NULL,
    alias TEXT NOT NULL,
    PRIMARY KEY (extension, alias),
    FOREIGN KEY (extension, canonical) REFERENCES window_functions(extension, name)
);
```

Per-row metadata is sparse — only the per-row argument tuples
(`param_types_json` = `[[arg0_type, arg1_type, ...]]` for the
overload's single signature). No frame info, no init/step/inverse/
finalize pointers (those don't apply to whole-partition compute).

### Postgis window function inventory

| SQL name (interface DB)        | Upstream WIT (postgis-clustering)                            | Aliases                                                                   |
|--------------------------------|--------------------------------------------------------------|---------------------------------------------------------------------------|
| `st_clusterdbscan`             | `st-cluster-dbscan`                                          | `st_cluster_dbscan`                                                       |
| `st_clusterintersectingwin`    | `st-cluster-intersecting`                                    | `st_cluster_intersecting_win`, `st_clusterintersecting_win`               |
| `st_clusterwithinwin`          | `st-cluster-within`                                          | `st_cluster_within_win`, `st_clusterwithin_win`                           |
| `st_clusterkmeans`             | `st-cluster-kmeans`                                          | `st_cluster_kmeans`                                                       |

**Shared-name caveat** (worth flagging because it bites name-match):
3 of the 4 cluster functions ALSO appear in the `aggregates` table
under the un-suffixed names (`st_clusterintersecting`,
`st_clusterwithin`, `st_clusterdbscan`). The `*win` suffix is the
postgis convention for the WINDOW form; the un-suffixed form is the
AGGREGATE form. The upstream WIT (`postgis-clustering` interface)
has ONE function per algorithm — the SAME upstream is registered
twice in SQL (once as aggregate, once as window). The interface DB
rows differentiate; the codegen must dispatch correctly to one
upstream entry from both registration types.

### Mobilitydb window functions

Zero. The `window_functions` table in `mobilitydb-interface.sqlite`
is empty. `tint-moving-average` and `tfloat-temporal-correlation`
in `mdb-temporal-wasm/wit/temporal.wit` carry a `window-micros` /
`window-size` parameter but are scalar functions, not SQL window
functions.

## Proposed IR + classifier changes

### Phase 0 substrate (must land first)

#### Core IR

```rust
// crates/datalink-shim-codegen-core/src/interface_db.rs

/// Per-row return shape for a window function. Mirrors the
/// per-row-return shape in scalars, but classified at the window-
/// level (one classification per function, applied to every row's
/// emitted value).
pub enum WindowReturn {
    /// Per-row `option<u32>` (st_clusterdbscan).
    OptionU32,
    /// Per-row `u32` (st_clusterkmeans).
    U32,
    /// Per-row `geometry` blob (st_cluster{intersecting,within}_win).
    GeomBlob,
    // Future: WitValueRecord for temporal window functions, etc.
}

pub struct WindowShape {
    /// Per-row argument signature (same shape as ScalarShape's
    /// params). Drives per-row argument decode at the dispatch site.
    pub row_arg_shapes: Vec<ParamShape>,
    /// Per-row return shape.
    pub returns: WindowReturn,
    /// Upstream interface function reference: which postgis-clustering
    /// entry (or future temporal-window entry) computes the partition.
    pub upstream: UpstreamRef, // existing IR
    /// Order-sensitive flag. Postgis cluster functions are
    /// position-stable; if `ORDER BY` was specified the host
    /// hands rows in that order. Mirror SQLite's
    /// `is_order_sensitive` flag from `aggregates` schema if/when
    /// the window_functions interface DB schema grows it.
    pub order_sensitive: bool,
}
```

The `WindowShape` is a NEW IR variant, NOT a flag on
`AggregateShape`. See **DD1** below for the rationale.

#### Interface DB loader

```rust
// crates/datalink-shim-codegen-core/src/interface_db.rs

pub struct WindowFunctionEntry {
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub param_signatures: Vec<Vec<TypeName>>, // single overload
}

pub fn load_window_functions(conn: &Connection, ext: &str)
    -> Result<Vec<WindowFunctionEntry>>;
```

Matches the `shim-bridge-codegen-core::plan::WindowFn` shape so the
downstream BridgePlan stays compatible.

#### Classifier

```rust
// crates/datalink-shim-codegen-core/src/interface_db.rs

pub fn classify_window_shape(
    win:         &WindowFunctionEntry,
    overrides:   &WindowOverrides,        // analog of aggregate overrides
    walker:      &WitWalker,
    name_match:  &NameMatcher,
) -> Result<WindowShape, ClassifyError>;
```

Pipeline:
1. Resolve canonical SQL name → upstream WIT function via the
   existing W1 name-matcher walker (postgis prefix-strip rules apply
   — `st_clusterintersectingwin` → strip `win` suffix → match
   `st-cluster-intersecting`).
2. Inspect upstream `func(list<borrow<X>>, ...) -> list<Y>` shape;
   verify `list<borrow<...>>` first param.
3. Map upstream `list<Y>` → `WindowReturn` variant by inspecting
   `Y`.
4. Map upstream per-row extra args (`f64`, `u32`, etc.) into
   `ParamShape` values.
5. Emit `WindowShape`.

#### Override table

Same architectural slot as `aggregate_function_overrides`:

```rust
pub struct WindowOverrides {
    /// SQL name → upstream interface entry, for name-match misses.
    pub by_sql_name: HashMap<String, UpstreamRef>,
}
```

Empty for the postgis pilot — the name-match walker handles all 4
functions via standard prefix-strip rules.

### Phase 1 — sqlite-emit pilot for ONE window function

Pick `st_clusterkmeans` as the pilot (`u32` return; simplest
shape).

Emit changes in `datalink-shim-sqlite-emit`:

1. **emit_lib.rs `push_aggregate_entry`**: drop the hard-coded
   `is_window: false`; thread a per-entry flag from the IR.
   Add a parallel `push_window_aggregate_entry` (or extend the
   helper) that emits an entry with `is_window: true`.

2. **emit_lib.rs `AggregateGuest` impl arms**: implement `value()`
   + `inverse()` for window functions:

   ```rust
   fn step(func_id: u64, context_id: u64, args: Vec<SqlValue>) -> Result<(), String> {
       // ... existing aggregate arms
       match func_id {
           <window_func_id> => {
               // Decode args[0] = geometry blob, args[1] = u32 (k)
               let geom_wkb = arg_blob(&args, 0, "st_clusterkmeans")?;
               let k = arg_u32(&args, 1, "st_clusterkmeans")?;
               push_window_row(context_id, geom_wkb, k);
               Ok(())
           }
           // ... other arms
       }
   }

   fn value(func_id: u64, context_id: u64) -> Result<SqlValue, String> {
       match func_id {
           <window_func_id> => {
               // Compute on first call, cache by context_id
               let labels = compute_or_get_kmeans_labels(context_id)?;
               // SQLite calls value() once per row in the partition,
               // walking left-to-right. We track a per-context cursor.
               let i = bump_window_cursor(context_id);
               Ok(SqlValue::Integer(labels[i] as i64))
           }
           _ => Err(stubbed("window value", func_id)),
       }
   }

   fn inverse(_func_id: u64, _context_id: u64, _args: Vec<SqlValue>) -> Result<(), String> {
       // No-op for whole-partition window functions — frame range
       // doesn't affect the precomputed labels.
       Ok(())
   }

   fn finalize(func_id: u64, context_id: u64) -> Result<SqlValue, String> {
       match func_id {
           <window_func_id> => {
               // Free the cached partition state.
               drop_window_state(context_id);
               Ok(SqlValue::Null)
           }
           // ... aggregate arms
       }
   }
   ```

3. **dispatch.rs helpers**: new
   `push_window_row` + `bump_window_cursor` + `compute_or_get_*`
   helpers, parallel to the `push_geom_state` / `take_geom_state`
   pattern from #547 raster aggregates. Per-context state lives in
   a `RefCell<HashMap<u64, WindowState>>` keyed by context_id.

Verification: `st_clusterkmeans` registered with `is_window: true`;
running `SELECT st_clusterkmeans(geom, 3) OVER (PARTITION BY ...)`
against a postgis-bridge build returns one cluster label per row.

### Phase 2 — duckdb-emit + datafission-emit for `st_clusterkmeans`

#### datafission-emit

The most direct fit. Changes in
`datalink-shim-datafission-emit/src/emit_lib.rs`:

1. Drop `WINDOW_STUB`; emit a real `window_function_registry::Guest`:

   ```rust
   impl window_function_registry::Guest for Component {
       fn list_functions() -> Vec<ftypes::WindowFunctionMeta> {
           vec![
               ftypes::WindowFunctionMeta {
                   name: "st_clusterkmeans".into(),
                   aliases: vec!["st_cluster_kmeans".into()],
                   param_types: <captured at codegen time>,
               },
               // ... other entries
           ]
       }

       fn return_type(name, _input_types) -> Result<LogicalType, _> {
           match name.as_str() {
               "st_clusterkmeans" => Ok(LogicalType::Integer),
               // ...
               _ => Err(FunctionError::UnknownFunction(name)),
           }
       }

       fn compute_partition(name, args_rows) -> Result<Vec<ScalarValue>, _> {
           match name.as_str() {
               "st_clusterkmeans" => {
                   let mut geoms = Vec::with_capacity(args_rows.len());
                   let mut k: Option<u32> = None;
                   for row in &args_rows {
                       geoms.push(decode_geom_from_scalar_value(&row[0])?);
                       if k.is_none() {
                           k = Some(decode_u32(&row[1])?);
                       }
                   }
                   let labels = postgis_clustering::st_cluster_kmeans(geoms, k.unwrap())?;
                   Ok(labels.into_iter().map(|l| ScalarValue::Integer(l as i64)).collect())
               }
               _ => Err(FunctionError::UnknownFunction(name)),
           }
       }
   }
   ```

2. **emit_wit.rs**: confirm world template already exports
   `window-function-registry@1.0.0` (verified — line 408 of
   `emit_wit.rs`).

#### duckdb-emit

Bigger lift because the v3 `aggregate-incr-dispatch` world isn't
currently exported by the codegen-generated bridges. Changes in
`datalink-shim-duckdb-emit`:

1. **emit_wit.rs**: extend the world template to import
   `aggregate-incr-dispatch` AND `runtime-ext` (whichever carries
   the `aggregate-incr-callback` registration). Use the
   `duckdb-extension-aggregate-incr` world variant from
   `~/git/ducklink/wit/duckdb-extension/worlds/duckdb-extension.wit:183`.

2. **emit_lib.rs**: add the `aggregate_incr_dispatch::Guest` impl
   with `call_aggregate_window`:

   ```rust
   impl aggregate_incr_dispatch::Guest for Component {
       fn call_aggregate_init(handle: u32) -> Result<u32, types::Duckerror> { ... }
       fn call_aggregate_update(handle, state, rows) -> Result<(), _> { ... }
       fn call_aggregate_combine(handle, target, source) -> Result<(), _> { ... }
       fn call_aggregate_finalize(handle, state) -> Result<types::Duckvalue, _> { ... }

       fn call_aggregate_window(
           handle: u32,
           partition: types::Rowbatch,
           frame: aggregate_incr_dispatch::WindowFrame,
       ) -> Result<types::Duckvalue, types::Duckerror> {
           let arm_idx = window_handle_table()
               .lock()
               .expect("window handle mutex poisoned")
               .get(&handle)
               .copied()
               .ok_or_else(|| types::Duckerror::Internal("unknown window handle".into()))?;
           // Cache partition by handle: drop on empty partition.
           if partition.is_empty() {
               drop_cached_partition(handle);
               return Ok(types::Duckvalue::Null);
           }
           let labels = compute_or_get_window_labels(handle, &partition, arm_idx)?;
           // Engine asks once per output row with the frame range;
           // the "current row" is unambiguous because partition[frame.start..frame.end)
           // contains the row whose result we emit. For ST_ClusterKmeans
           // (label per row), the result we want is labels[frame.start]
           // because each frame slice IS its own current row.
           Ok(types::Duckvalue::Int32(labels[frame.start as usize] as i32))
       }
   }
   ```

3. **register_aggregates() body**: window functions register through
   the same `aggregate-registry.register()` call as aggregates —
   the engine plan picks `call-aggregate` vs `call-aggregate-window`
   based on the SQL surface.

Verification: `mobilitydb-ducklink` window count stays 0 (no mdb
window funcs); `postgis-ducklink` window count: 0 → 1.
`postgis-datafission` window count: 0 → 1.

### Phase 3 — fan out to remaining 3 postgis window functions

Each subsequent function adds a per-target arm with the per-function
upstream call. Suggested order (by structural similarity to the
pilot):

1. `st_clusterdbscan` (returns `option<u32>`; needs NULL plumbing)
2. `st_clusterintersectingwin` (returns `geometry` blob; reuses
   geom encode logic from Phase 0/1 aggregates work)
3. `st_clusterwithinwin` (same shape as intersectingwin)

Final verification: all 4 postgis window functions register +
dispatch on all 3 targets. Counts:

| Target       | Before | After |
|--------------|-------:|------:|
| sqlite       |      0 |     4 |
| ducklink     |      0 |     4 |
| datafission  |      0 |     4 |

(Pre-existing postgis-sqlink-bridge may have register-only entries
without dispatch — verify byte-identity after Phase 1.)

## Per-target emit pattern (summary)

| Target       | Registration                                                                          | Dispatch arm                                            | Per-row decode site                          | Frame handling                                          |
|--------------|---------------------------------------------------------------------------------------|---------------------------------------------------------|----------------------------------------------|---------------------------------------------------------|
| sqlite       | Manifest entry `AggregateFunctionSpec { is_window: true }`                            | `step` (buffer) + `value` (cached read) + `inverse` no-op + `finalize` (drop) | per-row at step time (`SqlValue` → geom/u32) | streaming; per-context cursor walks the cached results  |
| duckdb       | Active `aggregate-registry.register(...)` + `aggregate-incr-dispatch` world export    | `call_aggregate_window(handle, partition, frame)`       | once per partition (lazy, cached by handle)  | engine resolves [start,end); reads cached result[start] |
| datafission  | Passive `window-function-registry.list-functions()` advertises                        | `compute_partition(name, args_rows)`                    | once per partition (eager, full fan-out)     | NOT exposed; whole-partition compute                    |

## Phase sequence

### Phase 0 — IR substrate (must land first)

- New `WindowShape` + `WindowReturn` IR in `interface_db.rs`.
- `load_window_functions` interface DB loader.
- `classify_window_shape` classifier.
- `WindowOverrides` table (empty for the pilot).
- Plumb through `BridgePlan` so emit crates see classified shapes.

Verification: regen postgis + mobilitydb bridges byte-identical
EXCEPT for the new window-function entries (gated on
`window_functions: Vec<WindowShape>` in the bridge plan being
consumed by emit crates).

### Phase 1 — sqlite-emit pilot (`st_clusterkmeans`)

- Emit window-mode manifest entry.
- Emit `value()` + `inverse()` arms for the pilot.
- Emit `push_window_row` / `bump_window_cursor` thread-local
  helpers in dispatch.rs.

Verification: postgis-sqlink-bridge regen byte-identical for
non-window code; window registration appears in manifest;
`SELECT st_clusterkmeans(geom, 3) OVER (...)` round-trips.

### Phase 2 — duckdb + datafission emit (same pilot)

- duckdb-emit: vendor `aggregate-incr-dispatch.wit`, extend world
  template, emit `aggregate_incr_dispatch::Guest` impl with
  `call_aggregate_window` arm + partition cache.
- datafission-emit: replace `WINDOW_STUB` with real
  `window_function_registry::Guest` impl with
  `compute_partition` arm.

Verification: postgis-ducklink + postgis-datafission window count
0 → 1 each.

### Phase 3 — fan out to remaining 3 functions

Each function is one row in `interface_db.window_functions` +
matching `postgis-clustering` upstream entry + 3 target emit arms.

Verification: all 4 functions wired on all 3 targets.

## Locked architectural decisions

### DD1. Separate `WindowShape` IR variant (NOT `is_window` flag on `AggregateShape`)

Reasoning:
- Of the 4 postgis window functions, 1 (`st_clusterkmeans`) has NO
  aggregate counterpart, 3 share a SQL function name and upstream
  but register differently. The "windowness" is per-SQL-registration,
  not per-upstream-WIT-function.
- The dispatch shapes diverge sharply across targets (sqlite:
  buffer-and-peek; duckdb: per-frame slice; datafission: whole-
  partition fan-out) — none of these reuse the init/step/merge/
  finalize aggregate state machine.
- The postgis-clustering upstream interface is `func(list<X>) ->
  list<Y>` (whole-partition compute), which is NOT an aggregate
  shape. Aggregates are `func(list<X>) -> single Y`. Sharing
  `AggregateShape` would muddy the `AccKind` semantics.
- IR is cheap. Per-shape classifier + per-shape emit arm is the
  established pattern (#547/#548/#590/#598).

### DD2. Whole-partition compute is the canonical window model; sqlite gets an adapter

The 4 postgis cluster functions ARE whole-partition. So is the
forthcoming MEOS `tcount`-as-window family. So is any spatial
window function we're likely to add. **Lock: codegen's WindowShape
assumes whole-partition semantics**; the sqlite-emit `value()` arm
is the adapter (buffer-at-step, compute-on-first-value, walk-cursor-
on-subsequent-value, drop-at-finalize).

Streaming-frame windows (LAG / LEAD / RANK / SUM OVER frame /
running averages) stay built into the host DB; this codegen does
NOT target them.

### DD3. `postgis-clustering` upstream interface is the ONLY upstream WIT family in the postgis pilot

No new upstream interface needed. The codegen name-matcher's
existing W1 walker handles `_win` suffix-strip (or we add the rule
to the postgis name-match overrides).

### DD4. DuckDB requires v3 `aggregate-incr-dispatch` world export

The currently-emitted DuckDB bridges export the `core` aggregate
dispatch path (`callback-dispatch.call-aggregate` whole-row-set
fold). Adding window function support requires extending the world
template to ALSO export `aggregate-incr-dispatch`. This is additive
— existing aggregate dispatch keeps working unchanged, the new
arms only fire when the engine plans a window query.

### DD5. Per-target state carriers

| Target       | State carrier                                                          |
|--------------|------------------------------------------------------------------------|
| sqlite       | thread-local `HashMap<u64, WindowContext>` keyed by `context_id`       |
| duckdb       | `HANDLE_TABLE`-extension carrying `HashMap<u32, WindowCache>` per arm   |
| datafission  | No state carrier — `compute_partition` is stateless per call           |

This mirrors DD3 from PLAN-aggregate-substrate.md. The handle-table
machinery from #611 extends with one more `AccState` variant for
window contexts.

### DD6. Window registration through the existing aggregate-registry on DuckDB

DuckDB has no separate `window-registry` resource. Window functions
register via `aggregate-registry.register(name, args, returns,
callback, options)` — the engine query planner picks dispatch arm
based on SQL surface. The codegen registers EVERY window function
twice if it ALSO has an aggregate registration (different SQL names
under the `_win` convention), pointing at distinct handles.

## Open questions

### OQ1. Does sqlite's `value()` arm definitely get called once per row in the partition?

The plan's sqlite adapter assumes SQLite drives `value()` once per
row in the OVER (PARTITION BY ...) frame, in row order, so the
per-context cursor walks left-to-right. Verify by reading the
sqlite3 window-function API docs (`sqlite3_create_window_function`)
or testing on the host. If `value()` is called differently (e.g.,
sometimes skipped, or revisited), the cursor-bump strategy breaks
and a frame-aware mapping is needed.

### OQ2. DuckDB partition caching ownership

The `aggregate-incr-dispatch` WIT comment says the component MAY
cache `partition` keyed by `handle`. Does the engine guarantee the
SAME `partition` rows on every frame call within a partition?
(Comment says yes — "the engine passes the same rows for every
frame of a partition.") Does the empty-partition signal arrive
between partitions reliably? Validate by reading ducklink-loader's
side or testing.

### OQ3. ORDER BY semantics

Datafission's `compute-partition` advertises that `args-rows` is in
`ORDER BY` order when one is specified. The postgis cluster
functions are not order-sensitive — they cluster geometries
regardless of input order. But future window functions (e.g., MEOS
`tcount`) may be order-sensitive. Lock: codegen IR carries
`order_sensitive: bool` on `WindowShape` even though the postgis
pilot doesn't use it.

### OQ4. Should the codegen emit a `register_window_functions()` glue
function on duckdb-emit?

Today the duckdb-emit has `register_scalars()` and
`register_aggregates()`. Window functions register through
`register_aggregates()` (same path) per DD6, OR they get their own
`register_windows()` for clarity. Decide when implementing Phase 2.
Recommendation: piggyback on `register_aggregates()` and pass a
flag.

### OQ5. Multi-extension worlds: handling aggregate + window co-occurrence

3 of the 4 postgis cluster functions have BOTH aggregate and
window registrations (different SQL names, same upstream). Make
sure the codegen emits BOTH registrations (the aggregate path
through `AccKind::Geom` from #607 + the window path through this
plan's new substrate). Bridge code de-duplicates the upstream call
by reusing the same upstream entry from both arms.

### OQ6. Are window function aliases in scope for Phase 1?

The interface DB carries 6 aliases across 4 canonical names.
sqlite + duckdb registration loops over all aliases (each gets a
separate registration with the same handle). datafission's
`list-functions` returns aliases inline in
`WindowFunctionMeta.aliases`. Same pattern as scalars. No extra
substrate.

## Risks

- **SQLite window-function ABI assumptions.** OQ1 is load-bearing
  for the sqlite-emit pilot. If `value()` isn't called per-row in
  the expected order, the cursor-bump adapter breaks and we have
  to redesign the sqlite path (worst case: skip sqlite-emit for
  whole-partition windows and route through a `vtab` UDTF
  approximation).

- **DuckDB v3 world adoption.** Adding `aggregate-incr-dispatch`
  to the codegen's world template may force a contract version
  bump for ducklink-loader on the host side. If the ducklink-loader
  doesn't already wire the v3 arms, this is a multi-component
  coordination — not just a codegen change.

- **Partition memory bloat.** A large window query on duckdb caches
  the WHOLE partition keyed by handle. For 1M-row partitions ×
  ~100B per row = ~100MB cache. Acceptable for the pilot but worth
  noting before fan-out.

- **Order-sensitive future extensions.** The MEOS `tcount` family
  (if added later) is order-sensitive and SQLite's window driver
  may need ORDER BY threading that the current pilot skips. Defer
  until a concrete consumer surfaces.

- **Name-match aliasing.** The `_win` suffix-strip rule for postgis
  may collide with other naming patterns. Verify the existing W1
  walker (`datalink-shim-codegen-core/src/name_match.rs`) handles
  this without false positives.

## Sequencing with other in-flight work

- **#607 / #611 / #612** (aggregate substrate) — landed. This plan
  reuses the per-target HANDLE_TABLE pattern + record codec
  registry established there.
- **#608** (st_3dextent / st_coverageunion / st_extent) — name-
  match work for aggregate path. Same name-match infrastructure
  applies to window classifier; no conflicts.
- **#609 / #610** (upstream postgis-wasm WIT gaps) — independent.
- **#617 / #619 / #620 / #621** (DuckDB pragma + datafission spatial-
  index / system-catalog / index-plugin dispatch) — independent.
  Window emit can land in parallel.

## References

### Contract WIT
- `~/git/sqlink/sqlite-loader-wit/wit/guest.wit:53-62, 263-284` —
  `aggregate-function-spec.is-window` + `aggregate-function`
  interface (step / finalize / value / inverse)
- `~/git/ducklink/wit/duckdb-extension/aggregate-incr-dispatch.wit` —
  v3 `call-aggregate-window(handle, partition, frame)` (the entire
  file is the relevant surface)
- `~/git/ducklink/wit/duckdb-extension/runtime.wit:51-58` —
  `aggregate-registry.register(...)` (shared registration path)
- `~/git/ducklink/wit/duckdb-extension/types.wit:125-128` —
  `invokeinfo.iswindow` flag (scalar-side; unrelated to custom
  windows)
- `~/git/datafission/wit/function-plugin/world.wit:166-182, 286-312,
  377-380` — `window-function-meta`, `window-function-registry`
  interface, `window-function-plugin` world

### Upstream WIT (postgis pilot)
- `~/git/postgis-wasm/wit/clustering.wit` — all 4 cluster functions
  (st-cluster-dbscan / -kmeans / -intersecting / -within)

### Interface DBs
- `/tmp/postgis-interface.sqlite` — `window_functions` table
  (4 rows: st_clusterdbscan, st_clusterintersectingwin,
  st_clusterwithinwin, st_clusterkmeans); `window_function_aliases`
  table (6 alias rows)
- `/tmp/mobilitydb-interface.sqlite` — `window_functions` table
  (0 rows)

### Current emit state (all 3 targets stubbed)
- `~/git/datalink/crates/datalink-shim-sqlite-emit/src/emit_lib.rs:
  960-970, 1687-1694` — `value()` / `inverse()` stub + hard-coded
  `is_window: false`
- `~/git/datalink/crates/datalink-shim-duckdb-emit/src/emit_lib.rs:
  ~492-509` — `call_aggregate` exists, NO `aggregate-incr-dispatch`
  world export at all
- `~/git/datalink/crates/datalink-shim-datafission-emit/src/emit_lib.rs:
  585-586, 1505-1522` — `WINDOW_STUB` writes empty
  `window-function-registry::Guest`

### IR state
- `~/git/datalink/crates/datalink-shim-codegen-core/src/interface_db.rs` —
  no `WindowShape` IR
- `~/git/shim-bridge-codegen-core/src/plan.rs:35,87-91` — downstream
  `WindowFn { canonical_name, aliases, param_signatures }` (data
  carrier, no classifier)
- `~/git/shim-bridge-codegen-core/src/load.rs:142-156` —
  `load_window_functions` SELECT from interface DB

### Architectural precedents (re-use these patterns)
- `~/git/sqlink/docs/plans/PLAN-aggregate-substrate.md` — same
  problem shape, one year newer ABI surface; the IR-then-emit
  phasing and per-target handle-table pattern transfers verbatim
- `~/git/sqlink/docs/plans/PLAN-shim-codegen-datalink-migration.md` —
  the α architecture (3 targets, what each crate owns) this work
  extends
