# Phase 3 — Retiring the bespoke `loaded::*` extension-loader (execution plan)

Status (2026-07-02): Phase 0 MERGED (`b4dff16e`, provider-backed dispatch, all
declarative tiers, provider-preferred + bespoke fallback, 22/22 tests). Phase 1
(#227) declarative rollout: **191/216 provider artifacts built + published** to
`ext.sqlink.dev/providers/` (+ `providers/manifest.json`), automation at
`deploy/providers/rollout.sh`. Phase 2: 213/213 valid components. **Phase 3
(delete the bespoke path) is blocked below — it is NOT a finishing step.**

## Why Phase 3 can't just "delete the path"
The bespoke `loaded::*` fallback is load-bearing for a real tail that the
declarative provider rollout provably cannot cover:

| Tail | # | Why not declarative-provider-backable |
|---|---|---|
| reentrant-SPI (import `sqlite:extension/spi`) | 14 | need a **reentrant provider world** (imports `compose:dynlink/linker`, exports/satisfies `spi`) — the reentrant tier **#220**, not built. `provider/wit/world.wit:73` says so explicitly. |
| http / dns | 2 | need **endpoint providers** (import `sqlite:extension/{http,dns}`); no shape exists. |
| CLI / dotcmd (pure wasi:cli, no `sqlite:extension`) | 6 | the **composed-CLI** tier, not resident providers. |
| non-wasm (`compress`, `zstd`) | 2 | do **not compile to wasm** — can NEVER be provider-backed. **100% coverage is physically impossible**, so fallback-deletion is impossible unless these are dropped from the catalog. |

## The path (each a real step, in order)
1. **#220 — reentrant provider tier.** Add `world provider-reentrant-*` to
   `woco .../provider/wit/world.wit`: `export endpoint; export
   sqlite:extension/spi; import compose:dynlink/linker; import metadata; import
   <tier>-function`. Implement the `spi` export in the provider `src` by
   re-entering the engine provider through `compose:dynlink/linker` (the host
   already threads the dynlink bridge into the resident provider — see
   `host/src/lib.rs` ~L9108, #225/#228). Build reentrant providers for the 14
   SPI exts; validate they dispatch + re-enter (extend `provider_backed_dispatch`
   / `resident_provider_coherence` tests). This is the hard, net-new piece.
2. **Endpoint providers for http/dns** (2). Resident endpoint providers over the
   `sqlite:extension/{http,dns}` surface (reuse the s3/http resident-provider
   pattern already in the host).
3. **CLI tier** (6). Confirm the composed-CLI path covers them (it already runs
   `composed_cli_matrix`); if so they never needed the resident-provider tier.
4. **Non-wasm decision** (`compress`/`zstd`). Either drop from the catalog, or
   accept a permanent non-wasm exception (⇒ the bespoke path can't be *fully*
   deleted — only reduced to these).
5. **Delete** (gated on 1–4 = 100% coverage): remove `loaded::*` worlds,
   `make_loaded_*_linker`, `LoadedState` SPI impls, and retire
   `sqlite-loader-wit`'s loader/policy/spi/resolver interfaces. High-risk refactor
   of the 15k-line `host/src/lib.rs`; do behind the full provider-matrix suite.

## Recommendation
The bespoke path is already correctly reduced to the tail (provider-preferred).
Do #220 (step 1) as its own reviewed PR — it's the true unblock. Steps 2–4 are
smaller; step 5 only after 100% (or a conscious non-wasm carve-out).
