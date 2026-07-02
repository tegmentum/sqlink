
## CORRECTION (2026-07-02): #220 is HOST-SIDE, not a provider shape

Deep investigation revised the #220 design twice, arriving at the correct one:
- **NOT 136 functions** — the `spi` interface is 18 funcs; reentrant exts import
  only `spi` (call ~2-5: `execute`, `execute-batch`).
- **NOT a provider "reentrant shape"** — a provider that *exports* `spi` while
  *importing* `scalar-function` from the same ext is an instantiation **cycle**,
  which the component model forbids. (POC shape built + compiled anyway on
  `woco feat/220-reentrant-provider @56c2a3c8` — it proved the forward logic /
  linker binding / CBOR envelopes; that code ports directly into the host impl.)
- **CORRECT: host-side SPI provision.** The reentrant exts compose with the plain
  **vtab declarative shape** (they export `metadata`+`scalar-function`+`vtab`),
  leaking `spi` as a host-satisfied import (exactly like `types`/`policy`/
  `metadata`). The host then satisfies `spi` on the resident-provider store.

### Exact host-side steps (sqlink/host/src/compose_provider.rs)
1. The reentrant resident linker already does `wasi::add_to_linker_async` +
   `compose::dynlink::linker::add_to_linker` (~L545-559), and `ProviderState`
   carries `dynlink_bridge: AsyncDynLinkBridge<HostWrapBackend>`.
2. Add `sqlite:extension/spi` to the resident-provider generate! world so the
   `spi::Host` trait + `add_to_linker` are generated.
3. Impl `spi::Host` for the provider store: forward `execute`/`execute-batch`/
   `changes`/`last-insert-rowid`/… to the engine via
   `bridge.resolve_by_id("engine").invoke("<method>", cbor)`, stub the rest.
   Reference: `LoadedState`'s existing `spi::Host` (host/src/lib.rs:4273) — same
   surface, redirected from the in-process conn to the engine provider.
4. `add_to_linker` that `spi::Host` on the reentrant branch (~L559).
5. Rollout (`deploy/providers/rollout.sh`): for exts importing `spi`, pick the
   `vtab` (or scalar) shape and DROP the "spi leftover = PLUGFAIL" rule — spi is
   host-provided. Rebuild the 14 reentrant exts as providers.
6. Test `define`/`completion` end-to-end (SQL re-entry through the provider).

Then: endpoint providers for http/dns (2), a catalog decision for the 2 non-wasm
exts (`compress`/`zstd`), and finally the `loaded::*` deletion refactor. #220's
core is a focused host-side change (~a day), with compiling POC logic behind it.
