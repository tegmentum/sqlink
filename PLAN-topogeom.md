# Plan: bridge TopoGeometry (last unbridged postgis surface)

## Goal

Wire `postgis-topology-topogeom` — the 6-fn resource interface
that's the only piece of postgis-wasm left unbridged. After this
the bridge covers everything except the intentionally-deferred
`postgis-batch` (70 fns, doesn't map to scalar SQL).

## Current state

Already shipped (commit `f7a4839`):
- `postgis-topology-edit` bridged via a thread-local
  `HashMap<u64, Topology>` (the "topology handle API"). Lifecycle
  is `st_topo_open` / `st_topo_serialize` / `st_topo_close`.
- All other topology read paths bridged BLOB-based.

What's left, upstream:

```wit
resource topo-geometry {
    topo-type: func() -> u32;             // 1=puntal, 2=lineal, 3=areal
    element-count: func() -> u32;
    get-elements: func() -> list<topo-element>;
    geometry: func() -> geometry;         // assembled MULTIPOINT/LINE/POLY
    clear: func();
}

create-topo-geom: func(
    topo: borrow<topology>,
    topo-type: u32,
    elements: list<topo-element>,
) -> result<topo-geometry, topology-error>;
```

where `topo-element = { id: u32, element-type: u32 }`.

## Architectural questions to settle

### Q1. Lifecycle relative to the Topology handle?

Upstream stores `parts: Vec<GeoGeometry>` inside `TopoGeomState`
— it snapshots primitive geometries at create time, so the
TopoGeometry doesn't reference the parent Topology after that.

**Decision**: each `TopoGeometry` gets its own opaque u64
handle in a separate registry (`TOPOGEOM_HANDLES`). Creating one
needs a live `TOPO_HANDLES` entry to construct against, but
afterwards the topo-geometry survives the topology being closed.
No parent-child accounting needed.

### Q2. How does SQL pass the element list?

`list<{id, element-type}>` — there's no native SQL shape.

**Decision**: JSON array of `[id, type]` pairs, matching the
existing R4 reclass / R2 setvalues convention:

    st_topogeom_create(topo_h, 2, '[[1,2],[4,2],[7,2]]')

(Object form `[{"id":N,"type":T}]` is more readable but the
pair form is shorter and easier to parse.)

### Q3. How does `get_elements()` come out of SQL?

Same JSON shape, output mode:

    SELECT st_topogeom_elements(tg_h);
    -- => '[[1,2],[4,2],[7,2]]'

(Symmetric with the input. Object form would be marginally more
self-describing but doubles the byte count for no information.)

### Q4. Persistence?

Upstream doesn't expose `to_bytes` / `from_bytes` for
TopoGeometry. The structure IS losslessly described by
`(topo_type, elements, source_topology)` — and the source
topology is itself serializable via `st_topo_serialize`. A
caller wanting durability can save `(topology_blob, topo_type,
elements_json)` and rehydrate with `st_topo_open` +
`st_topogeom_create`.

**Decision**: no serialization API in v1. The lifecycle is
session-bounded. Document the rehydration pattern in the README.

## Phases

### Phase TG1 — bridge plumbing (~1 hr)

- Sync vendored topology.wit (already current — it ships the
  topogeom interface).
- Add `import postgis:wasm/postgis-topology-topogeom@0.1.0;` to
  `extensions/postgis-bridge/wit/world.wit`.
- Add `use bindings::postgis::wasm::postgis_topology_topogeom as pg_topogeom;`
  in `extensions/postgis-bridge/src/lib.rs`.
- Allocate function ids `FID_TOPOGEOM_* = 1260..1266` (8 ids).
- Add 7 `ScalarFunctionSpec` manifest entries.

### Phase TG2 — handle registry (~30 min)

Add to the existing `thread_local!` block:

```rust
static TOPOGEOM_HANDLES: RefCell<HashMap<u64, TopoGeometry>> =
    RefCell::new(HashMap::new());
static TOPOGEOM_NEXT_ID: RefCell<u64> = const { RefCell::new(1) };
```

Helper `with_topogeom_handle` mirroring `with_topo_handle`.
Separate id sequences so a topology id and a topogeom id with the
same numeric value are unambiguous in error messages.

### Phase TG3 — dispatch arms (~2 hr)

| SQL                                              | Returns       |
|--------------------------------------------------|---------------|
| `st_topogeom_create(topo_h, type, elements_json)`| INTEGER (handle) |
| `st_topogeom_close(tg_h)`                        | INTEGER (1 if removed) |
| `st_topogeom_type(tg_h)`                         | INTEGER (1/2/3) |
| `st_topogeom_element_count(tg_h)`                | INTEGER       |
| `st_topogeom_elements(tg_h)`                     | TEXT (JSON)   |
| `st_topogeom_geom(tg_h)`                         | BLOB (WKB)    |
| `st_topogeom_clear(tg_h)`                        | INTEGER (1)   |

Implementation notes:
- `create`: parse the JSON list, build `Vec<TopoElement>`, call
  `pg_topogeom::create_topo_geom(&topo, topo_type, &elements)`,
  store in registry, return id.
- `geom`: call `tg.geometry().as_wkb()`.
- Errors: bad JSON / bad handle / upstream `topology-error`
  bubble through the standard `format!()` channel.

### Phase TG4 — test + commit + docs (~30 min)

Smoke test:
- `st_topogeom_create(invalid_handle, 1, '[]')` → "no such
  topology handle"
- `st_topogeom_close(99)` → 0
- `st_topogeom_create(handle_to_bad_topo, 1, 'not-json')` → JSON
  parse error
- `st_topogeom_clear` on a fresh tg → 1 → `element_count` → 0

Happy-path needs a valid topology blob; same blocker as the
topology edit smoke tests. Error paths exercise the dispatch.

Update README "Topology" section with a TopoGeometry subsection
following the same pattern as the edit handle API. Update
`PLAN-sqlite-plugins.md` table totals.

## Total estimated effort

~3-4 hours. Substantially smaller than R6 (one new resource,
no upstream changes needed, snapshots make lifetime simple).

## Out of scope

- Persistent TopoGeometry. The session-bounded handle is the v1
  contract; persistence pattern is documented but not wired.
- Indexed lookup of TopoGeometries by source topology. The
  registry is a flat HashMap keyed by tg id; there's no
  parent-child accounting. If a caller asks "all topogeoms
  built from topology X", they need to track that themselves.
