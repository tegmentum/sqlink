# Plan: cache parsed Components across `.load` calls

## Goal

Eliminate redundant `Component::from_binary` parse + validate
(+ AOT-compile) work when the same wasm gets loaded multiple
times. For the postgis-bridge bundle (~100 MB composed) the
parse is **the** cold-start cost — 100-500 ms per `.load`. O1
caches the *instance* per loaded extension; re-loading the
same `.wasm` still pays the parse cost in full.

This plan introduces two cache layers, ordered by how much they
help and how invasive they are:

- **C1 (in-process Component LRU)**: keyed by blake3(bytes),
  values are `Arc<Component>`, lives for the host process.
  Saves the second-and-later parse within a session.
- **C2 (precompiled-blob cache in the database)**: serialize
  `Component` via wasmtime's `Engine::precompile_component`,
  stash the resulting flat bytes in the user db keyed by the
  same digest. Cross-session — saves the parse on cold start
  too. Substantially larger scope: format-version pinning,
  trust on the loaded blob (it's runnable code), schema
  migration.

The grants work (commit `9dd1248`) already plumbs `digest_hex`
through to the cli and stores it in `_capability_grants`; both
cache tiers piggyback on that field without re-hashing.

## Current state

`host/src/lib.rs:2102` `load_extension_from_bytes`:

```rust
let component = Component::from_binary(&self.engine, &bytes)
    .map_err(|e| anyhow!("compile {name_hint}: {e}"))?;
let digest = blake3::hash(&bytes).to_hex().to_string();
```

Every `.load` rebuilds `component` from scratch. The `Engine`
caches its IR-level pipeline output for a given (input bytes,
engine config) pair, but the `wasmtime::Component` Rust object
allocates fresh — and on first sight of a given bytes it pays
the full validate + cranelift compile cost.

LoadedExtension already owns one Arc-wrapped Component per
extension instance (line 671 `pub component: Component`); the
existing `cached_minimal / _stateful / _tabular` caches reuse
the same `Component` across calls. Re-loading a different
extension name pointing at the same bytes is what wastes work.

## Architectural questions

### C1 — in-process Component LRU

**Q1.1. Lookup key.**

blake3(bytes) is what we already compute and store. It's
content-addressed: identical bytes → identical Component is
safe. Using path-or-URI as the key would let two different
extensions share a Component when they shouldn't; using
digest closes that.

**Decision**: `HashMap<String, Arc<Component>>` keyed by
hex blake3.

**Q1.2. Capacity / eviction.**

Each entry holds a fully-compiled wasm Component. A 100 MB
postgis bundle compiles to a similar-or-larger Component
representation in process memory; we can't pin many of them.

**Decision**: simple LRU with a `max_entries = 4` default
(overridable via env). Aggressively small; the wins are at
small N anyway (loading 2-3 distinct bundles is the realistic
peak).

**Q1.3. Lifetime relative to LoadedExtension.**

LoadedExtension already holds `Component`. The cache adds a
second Arc. Drop semantics: when the LRU evicts but a
LoadedExtension still holds the Component, it stays alive
(Arc refcount). When the LoadedExtension is unloaded and the
cache has evicted, the Component drops.

**Decision**: cache entry is `Arc<Component>`. `.load`'s flow
becomes: hash → lookup → if hit, clone Arc, skip
`from_binary`. Each cache miss adds an entry; capacity
overflow drops the LRU tail.

### C2 — precompiled-blob cache in the database

**Q2.1. Format pinning.**

`Component::serialize` output is engine-version-specific.
Loading a precompiled blob with a different wasmtime version
than it was produced by errors at deserialize time. Plan
needs a `(wasmtime_version, target_triple)` tag on every row.

**Decision**: dedicated columns
`engine_version` (str) + `target_triple` (str). Lookup
requires both to match.

**Q2.2. Trust.**

A precompiled blob is RUNNABLE CODE — much more dangerous
than raw wasm bytes (which we at least re-validate every
load). If an attacker can write to the user's db, they can
own the host the next time `.load` hits the cache. Two
mitigations:

- HMAC the precompiled bytes with a host-local secret. The
  cache only trusts a row whose HMAC verifies. An attacker
  with db write access doesn't have the secret.
- Or: validate the blob's signature against a known anchor
  (Ed25519-style).

**Decision**: HMAC with host-local secret. Generate the
secret on first cli launch, persist to `~/.sqlite-wasm/cache-
hmac.key` (rw-------). Skip caching entirely if the secret
file can't be created.

**Q2.3. Schema.**

```sql
CREATE TABLE _component_cache (
    digest_hex      TEXT NOT NULL,
    engine_version  TEXT NOT NULL,
    target_triple   TEXT NOT NULL,
    precompiled     BLOB NOT NULL,
    hmac            BLOB NOT NULL,
    cached_at       TEXT NOT NULL,
    source_uri      TEXT,            -- where the bytes came from (display)
    last_used_at    TEXT,
    PRIMARY KEY (digest_hex, engine_version, target_triple)
);
```

Same table conventions as `_capability_grants`. LRU eviction
runs on `last_used_at` ordering.

### Both layers — invalidation on bytes change

If a user replaces a wasm file (digest changes), the grant
table warns ("bytes changed, run revoke") but the cache layer
just misses (different digest → different cache row, the old
row eventually evicts). No cross-table coordination needed.

## Phases

### Phase C1 — in-process Component cache (~half day)

- Add `Host.component_cache: Arc<Mutex<LruCache<String,
  Arc<Component>>>>` field.
- In `load_extension_from_bytes`: after computing digest,
  consult the cache; on hit, use that Arc; on miss,
  `Component::from_binary` + insert.
- `LoadedExtension.component` becomes `Arc<Component>` (was
  `Component`). Plumb through.
- Trivial perf gate: a "load then load" of the same bundle
  should be 100-500 ms faster on the second load.

### Phase C2 — precompiled cache in db (~1-2 days)

- Add `cli/src/component_cache.rs` mirroring `grants.rs`
  shape. ensure_schema / get / put / list / delete / evict.
- Generate HMAC secret on first use; persist to disk.
- `host` exposes a new `engine_version()` helper so the cli
  can query.
- `.load` flow (cli-side):
  1. hash bytes
  2. look up cache (digest, engine_version, target)
  3. on hit + HMAC verify: hand precompiled blob to host via
     a new WIT method `extension_loader.load_extension_precompiled(blob,
     opts)`. Host calls `Component::deserialize` instead of
     `from_binary`.
  4. on miss or HMAC fail: normal load path; after success
     ask host to serialize the component, HMAC + cache it.
- `.cache components` dot-commands: list / clear /
  evict-older-than / stats.

### Phase C3 — instrumentation (~half day)

- Surface load-time-breakdown to `.cache stats components`:
  cold parse, cache hits, deserialize cost.
- A `--no-component-cache` cli flag for benchmarking.

## Sequencing

Same gate as the perf plan used:

1. **Ship C1, measure**. The expected win is a small
   step-function — repeated loads in the same session. If
   real workloads don't do that, stop.
2. **Profile cold start**. If `from_binary` is dominant *and*
   the user's flow involves repeated cold starts (the cli
   exits between loads of the same bundle), do C2. Otherwise
   skip.
3. **C3 only after a real complaint about lack of
   observability**.

## Out of scope

- Cross-host blob sharing (e.g. an organizational cache
  server). Same shape as C2 with a fetched-blob source and
  network trust, not local trust. Plausible follow-up but
  not covered here.
- Component "warmup" (instantiate the cached Component once
  on cli launch). O1's `cached_minimal` already amortizes
  instantiation across calls; doing it eagerly buys little.
- Compressing precompiled blobs. Precompiled bytes are
  already mostly machine code; compressing them is a marginal
  size win at a meaningful CPU cost on every load.

## Total estimated effort

- C1: ~half day. Trivial in-process plumbing.
- C2: ~1-2 days. Schema + HMAC + new WIT method +
  serialize/deserialize plumbing.
- C3: ~half day.

~2-3 days end-to-end for the full plan. C1 alone is a few hours
and gives most of the available win for the common case
(within-session repeated loads).

## Risk

C2's main risk is the HMAC trust story being wrong. If the
secret leaks, an attacker can poison the cache to run
arbitrary code on the next `.load` matching the poisoned row.
The threat model assumption is "anyone who can write to the
user's `~/.sqlite-wasm/` already owns the user account" — if
that holds, C2's HMAC is sufficient. If a workload has a
hostile user db but a trusted host filesystem (e.g.
multi-tenant SaaS), C2 should be SKIPPED in favor of C1 only.

C1 has essentially no security surface — it caches inside the
host process, no on-disk persistence.
