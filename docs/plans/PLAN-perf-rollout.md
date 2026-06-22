# Plan: perf rollout + catalog completion + doc hygiene

> Status: drafted. Sequencing reflects effort vs leverage; each
> phase is independently shippable. Stop after any phase.

Four phases, ordered by leverage-per-hour. The first three close
out the perf push by spreading its wins across the catalog and
proving nothing regressed; the fourth corrects the published
record so future readers don't act on stale numbers.

| Phase | Scope | Effort | Dependencies |
|---|---|---|---|
| A | `fetch_batch` rollout to 8 read-only vtab extensions | ~half-day per ext, parallelizable | series template (shipped) |
| B | `stats` extension embed port (14 aggregates) | half-day | `register_aggregates` (shipped) |
| C | Regression smoke run + bench refresh | 1-2 hours | A + B landed |
| D | Doc hygiene (3 files) | 1-2 hours | C numbers |

---

## Phase A — `fetch_batch` rollout

### Goal

Bring the 7.3x WIT-path scan speedup we proved on `series` to the
other 8 read-only vtab extensions in the catalog. Each one gains
the same shape — a `fetch_batch` method that returns up to N rows
in a single WIT crossing instead of paying one crossing per cell
+ per row of bookkeeping.

### Per-vtab classification

The 8 candidates split cleanly by what they have at the start of
xFilter:

#### Class 1 — full result set already in cursor at xFilter time

These already accumulate every row in cursor state inside xFilter
(parse a JSON array, walk a trie, enumerate a vector, etc.).
`fetch_batch` is a thin wrapper: read N rows out of the existing
buffer, advance the cursor's index, return.

| Extension | Vtab name | Cursor shape | Effort |
|---|---|---|---|
| `listargs` | `listargs` | `Vec<SqlValue>` | 30 min |
| `vec_each` | `vec_each` | `Vec<f32>` | 30 min |
| `text-utils` | `prefixes` | `Vec<String>` | 30 min |
| `completion` | `completion` | `Vec<(String, i64)>` | 45 min |
| `time-series` | `gap_fill_series` | `Vec<String>` | 30 min |
| `trie` | `trie` | `Vec<String>` (sorted matches) | 30 min |

Pattern (template; tweak column shape per vtab):

```rust
fn fetch_batch(
    _vtab_id: u64,
    cursor_id: u64,
    max_rows: u32,
) -> Result<Vec<VtabRow>, String> {
    CURSORS.with(|m| {
        let mut cursors = m.borrow_mut();
        let Some(c) = cursors.get_mut(&cursor_id) else {
            return Err("…: cursor not open".to_string());
        };
        let mut out = Vec::with_capacity(max_rows as usize);
        while out.len() < max_rows as usize && c.idx < c.values.len() {
            out.push(VtabRow {
                rowid: (c.idx + 1) as i64,
                columns: alloc::vec![
                    // …columns in declared-schema order…
                ],
            });
            c.idx += 1;
        }
        Ok(out)
    })
}
```

Set `batched: true` in the `VtabSpec` literal alongside.

#### Class 2 — pre-loaded file data, walk a vec

| Extension | Vtab name | Cursor shape | Effort |
|---|---|---|---|
| `pmtiles` | `pmtiles` | `Vec<TileInfo>` + payload bytes | 1 hour |

Same shape as Class 1; the row materializer reads tile bytes via
`tile_bytes(&c.data, c.tile_data_offset, t)` per row.

#### Class 3 — pre-scored kNN result, walk a vec

| Extension | Vtab name | Cursor shape | Effort |
|---|---|---|---|
| `vec0` | `vec0` (read path) | `Vec<ScoredRow>` | 1 hour |

Cursor's `rows: Vec<ScoredRow>` is populated by xFilter via brute
/ IVF / HNSW / HNSW8 / LSH topK. `fetch_batch` walks it.

### Acceptance per extension

1. `batched: true` set in the extension's `VtabSpec`
2. `fetch_batch` impl returns Ok(empty) at EOF and propagates the
   schema's column order
3. Existing smoke passes unchanged (the per-row semantics still
   work — sqlite walks the cache transparently)
4. `wasm-tools component wit …component.wasm | grep fetch-batch`
   confirms the new method is exported

### Validation

A representative scan workload, run with `batched=true` and
`batched=false` for each ext, must show the same row count + same
aggregate value and a measurable improvement. We use `series`'s
template:

```
SELECT count(*), sum(value) FROM <vtab> WHERE …;
```

Target: ≥ 3x speedup at 100k-row scale for Class 1 extensions
(the easy cases); ≥ 2x for Class 2/3 where per-row column work
is heavier.

### Order of ports

Recommended: `listargs` first (smallest, validates the rollout
mechanics), then the rest of Class 1 in any order, then `pmtiles`
and `vec0` last. Total ~5 hours.

---

## Phase B — `stats` extension embed port

### Goal

14 deferred aggregates ported to the embed path via the existing
`register_aggregates` helper. Catalog gains stats coverage in the
embedded build, matching the rest of the aggregate tier.

### Scope

The full surface:

```
stddev_pop, stddev_samp, var_pop, var_samp,
median, percentile, mode, percentile_cont, percentile_disc,
skewness, kurtosis,
regr_slope, regr_intercept, regr_r2
```

(Plus `regr_count`, `regr_avgx`, `regr_avgy`, `regr_sxx`,
`regr_syy`, `regr_sxy` if they ship — `extensions/stats/src/lib.rs`
authoritative.)

Each aggregate is one `AggregateSpec` entry in the embed-side
SPECS table + one `step_state` + one `final_state` thunk that
delegates to the existing crate-level math.

Most of these share state — the regression family all need
`n, sum_x, sum_y, sum_x2, sum_y2, sum_xy`; the percentile family
all need a sorted `Vec<f64>`. Group into a few state structs
and reuse.

### Acceptance

1. `extensions/stats/src/embed.rs` registers all 14 aggregates
2. `cli/Cargo.toml` gains `embed-stats = ["dep:stats-extension"]`
3. `cli/src/lib.rs` calls `stats_extension::embed::register_into`
4. Cli composed with `--embed stats` runs:
   `SELECT median(x), percentile(x, 0.95) FROM (VALUES (1),(2),(3),(4),(5));`
   without `.load`
5. Embed-vs-WIT result parity verified for at least 3 aggregates
   on identical inputs (sanity)

### Cost

Half-day. Mechanical; the helper does the hard work.

---

## Phase C — Regression validation + bench refresh

### Goal

Confirm the perf push (rounds 1-3 + fuel + fetch_batch + SIMD)
didn't regress functional behavior, and capture the new numbers
so the doc record matches reality.

### Steps

1. **45 ext smokes:** `make ext-smoke-all` — every extension
   smoke runs against the post-perf cli. Any FAIL gets
   triaged; commit only after green.
2. **Cli smokes:** the 4 in `cli-smokes/` (`embedded_plus_load`,
   `page_size`, `session`, `wal`) — `python3 tooling/cli-smoke.py`.
3. **Full bench:** `python3 tooling/bench.py --cwasm` across
   `insert / read / agg / join / ext-scalar / embedded-scalar`
   at 1k / 10k / 100k. Capture median-of-5 numbers.
4. **vec0 micro-bench:** the 20k × dim-384 brute-kNN we used
   for SIMD verification; record steady-state ms.
5. **fetch_batch micro-bench:** for each vtab ported in Phase A,
   record batched vs per-row at 100k rows.

### Acceptance

- 45/45 ext smokes pass
- 4/4 cli smokes pass
- Bench table captured in a single commit's commit message + a
  scratch markdown for the doc refresh

### Cost

1-2 hours of running things; longer if a smoke fails and needs
fixing.

---

## Phase D — Doc hygiene (3 files)

### Goal

The published `PLAN-benchmarks.md` numbers are now ~25-35%
pessimistic, the `fetch_batch` contract has no extension-author
reference, and `PLAN-vtab-mutating.md` (if it exists) doesn't
note shipped state. Fix the record.

### D1 — `PLAN-benchmarks.md` refresh

Update the snapshot table with the Phase C numbers. Add a "perf
push 2026-06" subsection narrating what changed and why:
- pragma defaults + sqlite flags  ~5% steady-state
- pread/pwrite + LTO + memory_reservation  ~10% read-heavy
- SIMD enabled  flat on bench but ~3-4x on vec0 kNN
- fuel split  ~12% across the board
- fetch_batch  7x for WIT vtab scans (new workload column)

Add a `batched-scan` workload to the matrix so future readers
can reproduce.

### D2 — Extension-author fetch_batch reference

Add a section to `extension-patterns.md` or `tooling/templates/`
(whichever houses the extension-authoring guide today) covering:
- When to set `batched: true` (any read-heavy vtab whose cursor
  has the full result set in memory at xFilter time)
- The `fetch_batch` signature + the `VtabRow` shape
- Returning Ok(empty) for EOF
- The host's per-row fallback (so opting out costs nothing)

Include `series`'s impl as the canonical reference.

### D3 — Vtab-mutating + perf-push shipped-status notes

Add a header note to whichever PLAN docs reference the v1
"read-only vtab" or pre-perf numbers, pointing to:
- `b334c43` — vtab-mutating shipped
- `978190e` — v2 vtab contract (Shadow + Integrity + FindFunction)
- `d62ef61` — fuel split
- `6773484` — fetch_batch shipped

Check: `PLAN-extensions-followups.md`, `PLAN-gaps.md`,
`PLAN-vec-followups.md`, `PLAN-tooling-and-session.md`. Anywhere
they say "deferred" or "out of scope" about something that
actually shipped, add a one-line "**Shipped <commit>**" tag.

### Acceptance

- `PLAN-benchmarks.md` numbers within ±5% of latest bench
- A new author can read the doc and write a `fetch_batch` impl
  in 30 minutes
- No PLAN doc still claims something is "deferred" if it
  shipped

### Cost

1-2 hours.

---

## Suggested sequencing for one sitting

```
Phase A1 (listargs)    proof the rollout pattern compiles + smokes
Phase A2-7 (Class 1)   parallel-able if context is wide enough
Phase A8 (pmtiles)     1 hour
Phase A9 (vec0)        1 hour  Phase A complete
Phase B (stats)        half-day
Phase C (regression)   1-2 hours, blocks on A+B
Phase D (docs)         1-2 hours, uses C's numbers
```

Total: ~2 days of focused work.

## Out of scope (intentional)

Things this plan deliberately does NOT cover, with reasons that
sit on documented "speculative until a consumer asks" guidance:

- **Custom in-memory VFS** (whole-DB-in-RAM on open). Biggest
  remaining read-workload lever (read 100k still at 4.6x native,
  dominated by per-page wasi calls). Speculative — needs a real
  user who can't fit their DB in the 256MB page cache today.
- **Session Phase 2/3** (capture + apply via WIT host plumbing).
  Phase 1 (pure-function helpers) shipped. Phase 2/3 explicitly
  deferred in `PLAN-tooling-and-session.md` "ONLY if a concrete
  consumer materializes."
- **Writable consumer extensions** (excel, zipfile). Contract
  exists (vtab-mutating shipped). Design questions (atomic
  rewrite? buffer until COMMIT? file locking?) hinge on what a
  real user wants. CSV proves the path; clone it when asked.
- **Per-engine config knobs beyond what we've shipped**.
  Diminishing returns; bench's last lever (read) is bottlenecked
  on wasi, not wasmtime config.

These four together are several more weeks of work each, and
shipping them without a consumer driving the design is exactly
the failure mode the existing PLAN docs warn against.
