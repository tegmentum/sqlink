# Plan: vector-search follow-ups for vec0

> **Phase 1 status: shipped.** Polling-based online insert
> wired into both IVF and HNSW backends; vec0_refresh /
> vec0_delete scalars exposed; host scalar dispatch now routes
> to the tabular cache when the extension has vtabs, so vec0
> scalars share thread_locals with vec0 vtab callbacks. 9
> native unit tests pass; end-to-end smoke through the cli
> verifies INSERT-then-query surfaces new rows, tombstones
> filter results, and refresh both drops the cache and clears
> the tombstone set.
>
> **Phase 2 status: shipped.** Persistent IVF and HNSW
> indexes via postcard serialization into a `_vec0_index`
> shadow table. Build paths persist after first build;
> connect paths try-load before paying the rebuild cost;
> staleness check matches (source_count, source_max_rowid,
> format_version, backend). vec0_refresh / vec0_delete drop
> the persisted blob in lockstep with the in-process cache
> drop. Cross-session smoke confirms session 2 sees the same
> kNN ordering as session 1 with the persisted blob unchanged.

## Goal

Take the three-backend vec0 vtab from "good enough for v1 brute /
IVF / HNSW behind identical SQL" to "production-shaped for
embedded ANN". Four threads, ordered by leverage:

1. **Online insert** for HNSW so the cached graph isn't stale
   the moment the user runs another `INSERT INTO docs`.
2. **Persistent indexes** so re-opens skip the rebuild.
3. **Quantized backends** (int8 HNSW + binary LSH) so >1 GiB
   vector corpora fit in addressable memory.
4. **Companion extensions** that aren't vec0-specific but
   round out the SQL surface: zstd compression, carray bind,
   sqlean-define.

Phases 1-3 share the same vec0 internals; phase 4 is parallel
work. Each phase is independently shippable.

---

## Phase 1  Online insert for HNSW (~2 days)

### Why

vec0's build-once-cache shape is the load-bearing limitation
today. Per the README: "inserts to the source table after the
first query are invisible to the cached index". For embedded
workloads where the user appends rows continuously (logging
embeddings, journal-of-thought, content ingest), that means
queries silently miss the recent half of the corpus.

### Architecture

The HNSW algorithm itself already handles insert  the cached
graph's `insert()` is the same call `build()` makes per row.
What's missing is the **trigger** that picks up source-table
deltas and feeds them in.

Three trigger options, in order of complexity:

| Option | How |
|---|---|
| **Polling** | Every kNN query, run `SELECT count(*), max(rowid) FROM source` via spi; if either changed, fetch the new rows and insert. Cheap; only catches inserts, not updates/deletes. |
| **Update hook** | Register an update-hook on the source table during vec0 connect; push (rowid, op) into a per-instance queue; drain on next kNN query. Catches all three ops. Requires the host's `update-hook` world  the hooked extension shape we already ship. |
| **Explicit refresh** | New scalar `vec0_refresh('knn_table')` that rebuilds. Lowest engineering cost; pushes burden to the caller. |

**Decision**: ship polling + explicit refresh in v1. Update-hook
is a follow-on once we want delete/update tracking.

### Pieces

1. **State in `hnsw::Index`**: add `last_indexed_max_rowid: i64`
   and `last_indexed_count: usize` (`Default::default()` = 0).
2. **Pre-query refresh** in `hnsw_topk`:
   - `SELECT count(*), max(rowid) FROM source`
   - If `count > last_indexed_count`:
     - Compute the rowid range to fetch: `rowid > last_indexed_max_rowid`
     - `SELECT rowid, embedding FROM source WHERE rowid > ? ORDER BY rowid`
     - For each new row, call `hnsw::insert_one(&mut idx, rowid, vec)`
     - Update `last_indexed_*` after.
3. **Tombstones**: a `HashSet<i64>` on `Index` for soft-delete.
   When `vec0_delete('knn', rowid)` is called (new scalar),
   add rowid to the tombstone. `search` filters tombstoned
   rowids out of the result *before* truncating to k.
4. **`vec0_refresh('table_name')` scalar**: clears the cache
   entry for that instance_id  on the next query, the
   backend rebuilds from scratch. Cheap escape hatch when
   the polling heuristic is wrong.

### Open questions

- **Update detection without a hook?** Polling on `(count, max_rowid)`
  catches inserts cleanly. Updates (row mutated in place) need
  either a content hash (expensive) or the update-hook path.
  For v1, document "updates require a refresh".
- **Tombstone storage growth.** A long-lived process with
  frequent deletes accumulates tombstones forever. Periodic
  full rebuild reclaims; trigger via `vec0_refresh`.
- **HNSW insert is `O(log N * M)`.** For bursty inserts (10k
  rows arriving between queries) the per-query refresh
  freezes the query. Cap per-query insert budget; defer the
  rest to the next query. Documented limitation.

### Estimated effort
~2 days. Half a day per piece, plus a recall+latency smoke
that compares "rebuild from scratch" vs "online insert" on a
streaming workload.

---

## Phase 2  Persistent indexes (~3-4 days)

### Why

For datasets where the build cost exceeds the query cost over
the cli's session lifetime (e.g. 100k+ vectors taking >1 s to
HNSW-build), losing the index on cli exit is friction. A
sidecar-table persistence layer makes the cli start cold-but-
fast: load the serialized blob, skip the rebuild.

### Storage shape

Per-vec0-instance metadata + payload in two shadow tables that
vec0 creates at xCreate time (we don't have xCreate write
permissions to user db today; ride the spi.execute interface
the same way the kNN scan does):

```sql
CREATE TABLE _vec0_index_meta (
    vtab_name      TEXT PRIMARY KEY,    -- vec0 instance name
    source_table   TEXT NOT NULL,
    backend        TEXT NOT NULL,       -- 'ivf' | 'hnsw'
    backend_params TEXT NOT NULL,       -- JSON: M, ef_*, K, ...
    source_count   INTEGER NOT NULL,    -- source row count at build time
    source_max_rowid INTEGER NOT NULL,  -- max(rowid) at build time
    format_version INTEGER NOT NULL,    -- bump on any layout change
    built_at       INTEGER NOT NULL     -- unix epoch
);

CREATE TABLE _vec0_index_blob (
    vtab_name      TEXT PRIMARY KEY,
    payload        BLOB NOT NULL        -- postcard-encoded Index
);
```

### Serialization

Pure-Rust: `postcard` (no-std-friendly, compact, fast) over
`#[derive(Serialize, Deserialize)]` on `hnsw::Index` and
`ivf::Index`. Format version starts at 1; bumping invalidates
the cached blob and forces a rebuild.

The HNSW graph for 100k 384-D vectors:
- vectors: 100k * 384 * 4 = 150 MB
- per-node top layer: 100k * usize = 800 KB
- neighbor lists: avg 16 links * 1.5 layers * 4 bytes/u32 = ~10 MB
- rowids: 100k * 8 bytes = 800 KB
- **total ~160 MB BLOB**

Acceptable; SQLite handles BLOBs up to ~1 GB per row natively.

### Load path

At vec0 connect:
1. `SELECT * FROM _vec0_index_meta WHERE vtab_name=?`
2. If row found AND `format_version` matches AND
   `source_count == SELECT count(*) FROM source` AND
   `source_max_rowid == SELECT max(rowid) FROM source`:
   - Load `_vec0_index_blob`, postcard-decode, cache.
   - Skip the rebuild.
3. Otherwise: fall through to lazy rebuild.

### Save path

After every rebuild (lazy or explicit):
- Serialize, upsert into both meta + blob tables.
- Wrap in a transaction so the two stay in sync.

### Open questions

- **Where do the shadow tables live?** Same db as the user's
  data (simplest, makes backup/restore atomic) vs. separate
  index db (no clutter, requires a second `--db` arg). v1:
  same db. Add `pragma vec0_index_dir=...` later if anyone asks.
- **Atomic update.** If a write transaction during rebuild
  gets interrupted, we want to fall back to a rebuild on the
  next session, not load a half-serialized blob. The format-
  version + transactional upsert give us that.
- **`postcard` adds ~50 KB to vec0.wasm.** Tolerable; the
  scalar vec extension already pulls in serde_json (~70 KB).

### Estimated effort
~3-4 days. Half a day on the serialization API (postcard +
derive), one day on the staleness-detection logic, one day on
the load-then-fallback wiring in `ivf_topk` / `hnsw_topk`, half
a day on a smoke that proves round-trip + invalidation.

---

## Phase 3  Quantized backends (~3-5 days)

### Why

f32 vectors are 4x larger than int8 and 32x larger than binary.
For corpora where the f32 form doesn't fit (>~10M vectors at
384-D = 15+ GiB), quantization is the only path to in-memory
search. The scalar vec extension already exposes
`vec_quantize_int8` and `vec_quantize_binary`; the backends
make those storage shapes queryable through vec0.

### Two backends, separable

#### 3a. int8 HNSW

A HNSW graph whose stored vectors are int8 (i8) with a scale
factor per vector. Build cost: 1 quantization pass over the
source; everything else is the same algorithm with an int8
distance kernel. Memory: ~4x reduction vs f32 graph.

Distance kernel for int8:
- L2: sum of squared differences, scaled by `s_a * s_b`
  where `s_a, s_b` are per-vector scale factors (or use the
  squared-distance fact that scaling is monotonic and skip the
  rescale)
- Cosine: dot product / (norm_a * norm_b)  approximated via
  int16 accumulator to avoid overflow
- Recall hit: typically 1-3%

`Backend::Hnsw8 { m, ef_construction, ef_search }` variant; new
`hnsw_int8` module mirroring `hnsw` but with `Vec<Vec<i8>>` and
parallel scales `Vec<f32>`.

#### 3b. Binary LSH

Random-projection LSH against binary-quantized vectors. Build
cost: pick D random hyperplanes, hash each vector to a D-bit
signature, bucket by signature. Query: hash the query, look up
the closest-Hamming buckets, re-rank candidates with full-
precision metric. Memory: ~32x reduction.

Recall hit: 5-15% depending on dim + nprobe budget.

`Backend::Lsh { d_signature, n_probes }` variant; `lsh` module
with `Vec<(i64, u64)>` (rowid + signature when `d_signature <=
64`; otherwise pack into `Vec<u8>`). Random hyperplanes are
deterministic (same xorshift seed trick).

### Open questions

- **Quantize at build vs. at source-column?** Build-time
  quantization keeps the source schema unchanged (f32 BLOB)
  and lets the backend choose precision. Source-column
  quantization (i.e. user stores int8 directly) saves the
  pre-quantize pass but locks the storage format. v1: build-
  time quantization, source stays f32.
- **Mixed-precision re-rank.** Quantized HNSW finds candidates;
  optionally re-rank the top-`ef_search` with f32 (held
  alongside the int8 in the graph). Configurable via
  `rerank_full=true`. Doubles memory but recovers ~half the
  recall loss.
- **`binary LSH` is technically a different backend class.**
  Lump it under vec0 anyway since the SQL surface is the same;
  the implementation just doesn't share much code with HNSW.

### Estimated effort
~3-5 days. 2 days int8 HNSW (mostly mechanical: copy `hnsw`,
swap types, add scale-factor handling); 2 days binary LSH
(novel code, simpler algorithm); half a day docs+smokes.

---

## Phase 4  Companion extensions (~1-2 weeks; parallelizable)

Not vec0-specific; rounds out the broader SQL surface beyond
the three ANN backends. Independent of phases 1-3; can be
worked in parallel.

| Extension | Why | Effort |
|---|---|---|
| **sqlite-zstd** | Compress / decompress arbitrary BLOB columns. Pairs naturally with f32 embedding storage (typically 2-3x shrink). `zstd_compress(b)`, `zstd_decompress(b)`, and optionally a transparent-decompress vtab. | 2-3 days |
| **carray** | Bind a Rust slice into a prepared statement as a vtab so the host can pass arrays to vec0 (e.g. "kNN over THIS list of rowids"). Useful host-side; less useful from SQL alone. | 1-2 days |
| **sqlean-define** | User-defined SQL functions stored in the db. `define('add_one', 'SELECT $1 + 1')` produces an `add_one(x)` callable later. Reuses the existing scalar dispatch machinery. | 2-3 days |
| **wal2** | Alternative WAL mode  upstream sqlite enables via a compile flag. If our bundled compile picks it up cleanly, this is a one-line addition to `LIBSQLITE3_FLAGS` (~5 min); if not, deferred. | 5 min - 1 day |

These can run in any order; each is its own port-shape.

---

## Out of scope (intentional)

- **Distributed ANN.** Federated search across cli instances or
  sharded indexes. Embedded SQLite isn't the place for that;
  push to a separate plan if a use case emerges.
- **GPU kernels.** wasm32-wasip2 doesn't have a GPU path.
  Future wasm64 + WebGPU could; out of scope here.
- **Sparse vectors / SPLADE.** vec0 today assumes dense f32
  vectors. Sparse-vector kNN is a different algorithm class
  (inverted index over token IDs); deserves its own vtab
  (`vec_sparse0`) rather than retrofitting vec0.
- **Multi-tenancy / per-row encryption.** Encrypted storage is
  orthogonal to vec0 and would be handled at a lower layer.

---

## Sequencing recommendation

1. **Phase 1 first.** Highest user-visible impact  fixes the
   one limitation the README has to disclaim.
2. **Phase 2 second.** Re-open speedup compounds with Phase 1
   (the rebuild's not needed on re-open AND it stays fresh).
3. **Phase 3 third.** Only matters once corpora outgrow f32;
   the brute/IVF/HNSW trio is enough for ≤1M dim-384 vectors,
   which is most embedded use cases.
4. **Phase 4 anytime.** Independent; pick by user demand.

Total runway for phases 1-3: ~8-12 days of focused work.
Phase 4 is "as needed".
