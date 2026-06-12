# Plan: Compose-Orchestration Integration

## Overview

Add support for *Fiji functions* — tiny wasm components that resolve
shared providers at runtime via `compose:dynlink/linker` instead of
bundling a full SQLite runtime each. The goal is to amortize the
~2 MB rusqlite-bundled cost across many small functions, and to make
common libraries (text, hashing, encoding, …) reusable as shared
providers.

This is **additive** to everything that ships today. Existing
`sqlite:extension`-world extensions (test/agg/coll/hook/spi/auth/
live-spi) keep working unchanged. The new shape lives alongside.

## Reference project

`~/git/webassembly-component-orchestration/` (referred to below as
`compose-orchestration`). The pieces we'll consume:

- `wit/compose-dynlink/` — `endpoint`, `linker`, `dynlink-{guest,provider,endpoint-consumer}` worlds
- `wit/sys-compose/` — `types` (digest, error)
- `hosts/wasmtime/src/dynlink.rs` — reference Host impl we'll port the patterns from
- `libs/compose-core/src/blobs.rs` — their SHA-256 CAS (we already have blake3; coexistence detailed in CP7)
- `examples/dynlink-{echo-provider,endpoint-consumer}/` — reference shape for providers + consumers

## Architecture

```
                          ┌──────────────────────────────┐
                          │ sqlite-wasm-run (host)        │
                          │                              │
                          │  ┌────────────────────────┐  │
                          │  │ Host impl of           │  │
                          │  │ compose:dynlink/linker │  │
                          │  │   resolve-by-id        │  │
                          │  │   resolve-by-digest    │  │
                          │  └────────────┬───────────┘  │
                          │               │              │
       ┌──────────────────┼───────────────┘              │
       │                  │                              │
       ▼                  ▼                              │
  ┌─────────────┐   ┌──────────────────┐                 │
  │ fiji-hello  │   │ host-shim        │                 │
  │ (5 KB)      │   │ "sqlite-runtime" │                 │
  │ imports     │   │ provider         │◄────────────────┤
  │   linker    │   │  invoke("query"  │                 │
  │             │   │   ,cbor) → SQL   │                 │
  └─────────────┘   └──────────────────┘                 │
                                                          │
        rusqlite::Connection ◄──────────────────── owned  │
                                                          │
                          ┌───────────────────────────────┘
                          │
                          ▼
                 ┌──────────────────────┐
                 │ user db: file.db     │
                 │ shared by everyone   │
                 └──────────────────────┘
```

Three new concepts in the codebase:

1. **`compose:dynlink/linker` host impl** — sqlite-wasm-run learns
   to satisfy the `linker` interface for guests that import it.
2. **"sqlite-runtime" provider** — host masquerades as a
   `dynlink-provider` component. Its `endpoint.handle(method,
   payload)` dispatches `query` / `execute` / `execute-batch` calls
   to the host's `rusqlite::Connection`.
3. **`dynlink-guest`-world extensions** — Fiji functions. Tiny
   components that import `linker`, resolve `sqlite-runtime` at
   first call, invoke against the handle.

## Steps (CP1 — CP8)

### CP1 — Bring compose-orchestration WIT into our build (~half day)

Reference the orchestration project's WIT without forking it:

```
sqlite-wasm/
├── wit/
│   └── deps/
│       ├── sqlite-extension/          # existing submodule
│       ├── compose-dynlink/           # NEW — copy from ~/git/webassembly-component-orchestration/wit/compose-dynlink/
│       └── sys-compose/               # NEW — copy from .../wit/sys-compose/ (just types.wit)
```

Recommend **copy with attribution**, not submodule. Their packages
are at `compose:dynlink@0.1.0` and `sys:compose/types@1.0.0`, and
we want the freedom to vendor stable revisions independently of
their development cadence. A README in `wit/deps/compose-dynlink/`
notes the source commit + license.

Validate the WIT loads correctly under our existing wit-bindgen
setup by adding a stub bindgen in `host/src/lib.rs`:

```rust
pub mod compose {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "compose-host-stub",
        imports: { default: async },
        exports: { default: async },
    });
}
```

with a tiny `compose-host-stub` world that imports
`compose:dynlink/linker`. If it builds, CP1 lands.

**Acceptance:** `cargo build --release` of host succeeds with the
compose WIT included.

### CP2 — Implement `compose:dynlink/linker` on host (~2-3 days)

This is the load-bearing piece. The host trait shape (from their
WIT):

```rust
trait Host {
    async fn resolve_by_digest(&mut self, d: Digest) -> Result<Resource<Instance>, Error>;
    async fn resolve_by_id(&mut self, id: String) -> Result<Resource<Instance>, Error>;
}

trait HostInstance {
    async fn invoke(&mut self, h: Resource<Instance>, method: String, payload: Vec<u8>)
        -> Result<Vec<u8>, Error>;
    async fn drop(&mut self, h: Resource<Instance>) -> Result<()>;
}
```

For v1:

- `resolve_by_id` accepts hardcoded names: `"sqlite-runtime"`,
  `"std-text"`, `"std-hashing"`, etc. Maps to host-side shims (CP4)
  initially; later mapped to actual provider components.
- `resolve_by_digest` looks up in our blake3 cache OR the
  compose-orchestration SHA-256 blob store (CP7's bridge); if
  found and is a registered provider, instantiate.
- `Instance` resource carries `(provider_kind, provider_state)`.
  Provider state for "sqlite-runtime" is a clone of
  `Arc<Mutex<rusqlite::Connection>>` borrowed from the cli.

`invoke` dispatches by `(provider_kind, method)`. For
`sqlite-runtime` + `"query"`: decode CBOR payload as
`{sql: string, params: list<value>}`, run via the connection,
encode the result as CBOR `{columns, rows, changes, last_rowid}`.

Trust verification is policy-driven. v1: log every resolution,
accept everything signed by anyone OR unsigned (development
default). Production-grade trust is a follow-up; we want the
plumbing in place but not yet the cryptographic gates.

**Acceptance:** a unit test instantiates the linker, resolves
"sqlite-runtime", invokes `"query"` with a hand-rolled CBOR
payload `{"SELECT 1+1"}`, decodes the response, asserts `[[2]]`.

### CP3 — Define the sqlite-runtime endpoint protocol (~1 day)

The `compose:dynlink/endpoint.handle(method, payload)` shape is
opaque bytes both ways. Document our envelope explicitly so future
providers can be written against it.

**Method namespace** (all lower-kebab):

| Method | Payload (CBOR) | Response (CBOR) |
|---|---|---|
| `query` | `{sql: text, params: [value...]}` | `{cols: [text...], rows: [[value...]...], changes: u64, last-rowid: i64}` |
| `query-scalar` | ↑ same | `value` |
| `execute` | ↑ same | `{changes: u64, last-rowid: i64}` |
| `execute-batch` | `{sql: text}` | `{changes: u64}` |
| `prepare` | `{sql: text}` | `{stmt-id: u64}` |
| `step` | `{stmt-id: u64}` | `{done: bool, row: option<[value...]>}` |
| `finalize` | `{stmt-id: u64}` | `_` (empty CBOR `null`) |
| `manifest` | `_` | `{name: text, version: text, methods: [text...]}` (introspection) |

The `value` CBOR type follows the canon:cbor profile from
`sys:compose`. Mapping to our existing `SqlValue`:

```
null         → CBOR null
i64          → CBOR integer
f64          → CBOR float
text         → CBOR string
blob         → CBOR byte string
```

Document in `host/COMPOSE-PROTOCOL.md` alongside SPI-LIVE.md and
AGGREGATE-DISPATCH.md.

**Acceptance:** the protocol doc lands; CP2's unit tests use
exactly these methods + payloads.

### CP4 — Host-side "sqlite-runtime" provider (~2 days)

Not a wasm component — a host-side shim that masquerades as one.
The cost of building a real `dynlink-provider` component that
re-exports rusqlite is too high (2 MB binary, slow build, bundled
SQLite duplicates the host's), and unnecessary: from a Fiji
function's perspective the difference is invisible.

```rust
struct SqliteRuntimeProvider {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl Provider for SqliteRuntimeProvider {
    async fn handle(&self, method: &str, payload: &[u8]) -> Result<Vec<u8>, Error> {
        match method {
            "query"        => self.do_query(payload).await,
            "query-scalar" => self.do_query_scalar(payload).await,
            "execute"      => self.do_execute(payload).await,
            "execute-batch"=> self.do_execute_batch(payload).await,
            "prepare"      => self.do_prepare(payload).await,
            "step"         => self.do_step(payload).await,
            "finalize"     => self.do_finalize(payload).await,
            "manifest"     => self.do_manifest().await,
            other          => Err(Error::UnknownMethod(other.to_string())),
        }
    }
}
```

Reuses the cli's `Arc<Mutex<Connection>>` so Fiji functions see
the same data the cli is operating on — committed schema, current
state, post-`.open` consistency. *Live* state vs. committed state
follows the same hybrid as today's `spi.execute` / `execute-live`;
the protocol can grow `query-live` / `execute-live` methods later
without WIT changes.

**Acceptance:** `host::dispatch_compose_invoke("sqlite-runtime",
"query-scalar", cbor(&{sql:"SELECT 1+1"}))` returns `cbor(&2)`.

### CP5 — `fiji-hello` example function (~half day)

A minimal demonstrator under
`~/git/sqlite-wasm-loader/runtimes/wasmtime/fiji-hello/`. Target
`compose:dynlink/dynlink-guest` world:

```rust
wit_bindgen::generate!({ world: "dynlink-guest", path: "wit", generate_all });

use compose::dynlink::linker;

#[unsafe(no_mangle)]
extern "C" fn _start() {
    let sqlite = linker::resolve_by_id("sqlite-runtime")
        .expect("resolve sqlite-runtime");
    // CBOR-encode { "sql": "SELECT COUNT(*) FROM users" }
    let req = serde_cbor::to_vec(&Query { sql: "SELECT COUNT(*) FROM users".into() }).unwrap();
    let resp = sqlite.invoke("query-scalar", &req).expect("invoke");
    let count: i64 = serde_cbor::from_slice(&resp).expect("decode");
    println!("users: {count}");
}
```

Built with `cargo component build --release`. Expected size: a
few KB (no rusqlite, just `compose:dynlink` + `serde_cbor`).

**Acceptance:** the built component is < 50 KB. Loading it via the
cli runs `_start`, prints `users: <n>`.

### CP6 — cli-rust learns to recognize compose-shaped extensions (~1-2 days)

At `.load` time we currently always read the manifest via
`metadata.describe`. Compose-shaped guests don't export
`sqlite:extension/metadata`; they export `wasi:cli/run` (or are
reactor-shaped exporting a different entry point) and import
`compose:dynlink/linker`.

Detection: inspect the component's WIT via wasm-tools at load
time, or simpler — try `loaded_compose::DynlinkGuest::instantiate_async`
first, fall through to `loaded::Minimal::instantiate_async` second.

When the compose path matches:

1. The host adds the `linker` interface to the loaded extension's
   Linker (same Linker as today's loads, just with the new
   interface wired).
2. The host instantiates the component and calls its entry point.
3. Each `linker.resolve_by_id` call dispatches through CP2's host
   impl.

The cli prints `Loaded fiji function: fiji-hello (5 KB)` instead
of the manifest-driven `Loaded extension: ... (N registered)`.

For invocation, two paths to choose between:

- **A. Auto-run at load**, like a command-mode binary. The function's
  `_start` runs once; output goes to the cli. Good for one-shot
  functions.
- **B. Register as a dot-command**: `.load fiji-hello.wasm` makes
  `.fiji hello` available. The dot-command instantiates fresh on
  each call. Better for repeated invocation.

Recommendation: ship **A** first (one-shot). Add **B** when there's
a function people want to call repeatedly.

**Acceptance:** `.load fiji_hello.wasm` runs the function; output
is correct row count from the user's db.

### CP7 — CAS coexistence (~1 day)

The orchestration project uses SHA-256 in
`libs/compose-core/src/blobs.rs`. We use blake3 in
`host/src/cache.rs`. For now, both exist:

```
host::cache::Cache             our blake3 cache, unchanged
host::compose_cas::ComposeCas  thin wrapper around compose-core::blobs
                                (linked from their workspace via path =)
```

The `linker.resolve_by_digest` lookup tries both: if the digest is
64 hex chars (32 bytes), it's SHA-256 → ComposeCas; otherwise it's
blake3 (also 64 hex) and we can't tell from the digest alone. Carry
a `DigestAlgo` enum alongside.

Or simpler: every cache write computes BOTH hashes and stores
under both keys. The disk cost is one extra hardlink per artifact.
Lookups just try whichever matches.

A future commit unifies (probably on sha-256 since that's the
broader ecosystem). v1 dual is fine.

**Acceptance:** `.cache list` shows entries that work for both
digest-format `.load`s.

### CP8 — Documentation + validation (~half day)

- **ARCHITECTURE.md**: add the "Fiji functions" section showing
  the dispatch flow alongside the existing two CLIs.
- **AUTHORING-FIJI-FUNCTIONS.md** (new): step-by-step for writing
  a new Fiji function — Cargo.toml shape, WIT setup, `_start`
  body, how to build, how to invoke from the cli.
- **PLAN-outstanding.md**: mark compose integration done; capture
  any deferred items (real wasm-component providers, trust gating,
  unified CAS).
- **End-to-end test**: a smoke test that loads `fiji-hello`,
  invokes it, verifies output.

**Acceptance:** docs pass review; smoke test green.

## Risks

- **Their wasmtime version vs. ours.** They target some wasmtime
  version; we're on 45. If their `compose-core` requires newer/
  older, we either pin a compatible version or fork small pieces.
  Cheap to check; do it in CP1.
- **Resource handles in async bindgen.** Their `linker.instance`
  is a wasmtime resource. Async bindgen for resource exports has
  some sharp edges. If we hit issues, the fallback is opaque u64
  handles managed in host state (less idiomatic, fully works).
- **Trust gates.** Their model wants signature verification. v1
  ships permissive (log + allow). Production needs a story —
  separate plan after CP8 lands.
- **Two CAS systems** is unsatisfying long-term. CP7 picks the
  pragmatic shape; unification deserves its own decision once we
  have data on which digest format providers + consumers actually
  use most.

## Wins

| | Today | After compose integration |
|---|---|---|
| Tiny ext size | ~12 KB (rusqlite-bundled) + 2 MB shared SQLite in cli | < 50 KB; SQLite already loaded by cli |
| 100 small functions | ~1.2 MB total | ~5 MB total → mostly the shared cli, deduplicated |
| Common libs | duplicated per ext | shared providers, write-once |
| Capability surface for a Fiji function | `Spi` (broad) | `resolve("sqlite-runtime")` only (narrower) |
| Function authoring | requires understanding our entire extension WIT | one import, one trait method (`invoke`) |
| Cross-language | Rust-only for non-trivial extensions today | any language that targets `dynlink-guest` |

## Out of scope (named so they don't get assumed)

- Full sys:compose plan/emit pipeline (the `composectl` toolchain
  beyond what we directly need). Worth using later for build
  reproducibility.
- Audit / attest / metrics worlds. Orthogonal to the runtime
  linking story.
- Migration of existing `sqlite:extension`-world extensions to
  Fiji shape. They keep working unchanged.
- A wasm-native sqlite-runtime provider (vs. CP4's host shim).
  Useful conceptually but defers a real cost without observable
  benefit until we have providers that genuinely need cross-host
  portability.
- Multi-tenancy / namespacing of provider IDs. v1 has a single
  flat namespace (`"sqlite-runtime"`, etc.). Tenant-scoped IDs
  ship when there's a user.

## Branch strategy

One branch `feat/compose-integration`. Commits per CP step. Ship
internally green at CP6 (working `.load fiji_hello`); merge to main
after CP8.

Estimated total: **7-10 days**. CP1 + CP3 + CP5 + CP8 are quick;
CP2 + CP4 + CP6 + CP7 are the substantive chunks.

## Order of execution

```
CP1 (WIT plumbing, half day)
  ↓
CP3 (protocol doc, parallel with CP4) → CP4 (sqlite-runtime shim)
  ↓                                         ↓
CP2 (linker host impl) ◄──────────────────-┘
  ↓
CP5 (fiji-hello, half day)
  ↓
CP6 (cli-rust detection + load path)
  ↓
CP7 (CAS coexistence)
  ↓
CP8 (docs + smoke test)
```

CP3 + CP4 can run in parallel with CP2 once CP1 lands.
