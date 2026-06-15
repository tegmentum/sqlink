# Plan: Resolver Components + CAS Cache

> **Status: shipped — both halves.** Status check 2026-06-15:
>
> | Half | Evidence |
> |---|---|
> | Resolver components | `.register-resolver SCHEME PATH` / `.unregister-resolver SCHEME` cli dot-commands (`cli/src/lib.rs:286,1298,1337`); host's `register_resolver` / `unregister_resolver` (`lib.rs:2002,2020`); `.load <scheme>://…` routes through the URI path (`load_extension_from_uri`, `host/src/lib.rs:2089`); `looks_like_uri` triages on the cli side |
> | CAS cache | Split out into `PLAN-cas-cache.md` (commit `88653b0` closed CP8); `sqlite-cas-cache/` workspace member; full `.cache` dot-command family; v2 schema with blake3 + sha256 dual-key |
>
> Plan body kept intact for reference.

## Overview

`.load` today takes a filesystem path only. This plan extends it so
users can write `.load https://…` or `.load oci://…`, with the
download path going through a *resolver* — itself a wasm component,
sandboxed and gated by the same capability/policy system as every
other extension. A content-addressable local cache deduplicates
downloads and gives users a verification primitive (pinned hashes).

## Decisions locked in

| | |
|---|---|
| Resolver dispatch | Scheme-keyed map: URI scheme → exactly one resolver |
| Cache location | XDG per-user (`$XDG_CACHE_HOME/sqlite-wasm/extensions` or `~/.cache/sqlite-wasm/extensions`); overridable via `SQLITE_WASM_CACHE_DIR` env or `--cache-dir` flag |
| Bootstrap | Component from day 1 — first HTTP resolver is itself a wasm component |
| Hash | blake3 |

## Architecture

### Resolver as a component

A resolver is just an extension that exports one extra interface.
Same loader, same policy, same fuel + epoch limits. To use HTTP it
imports the existing `http` interface (already wasi-http-shaped) and
declares the `http` capability in its manifest, so the host's
`http_policy.allowed_hosts` automatically gates which URLs it can
hit. No new sandbox mechanism — the resolver is bound by the same
rules a query extension would be.

### CAS cache

Two layered key-value stores inside the cache dir:

```
$CACHE_DIR/
├── blake3/                       # immutable, content-addressed
│   └── aa/bb/aabb…ff.wasm        # split-by-prefix to keep dirs small
└── uri_index/                    # mutable, uri -> content hash
    └── <blake3(uri)>.json        # {uri, hash, fetched_at, etag?}
```

`.load <uri>` flow:

1. Parse URI scheme; if no scheme, fall through to the existing
   filesystem path code.
2. Hash the URI with blake3 → look up `uri_index/<urihash>.json`.
   If present and the referenced `blake3/<contenthash>.wasm` still
   exists, load it.
3. On miss: find the resolver for `scheme` (error if no resolver
   registered) → call `resolver.resolve(uri)` → hash returned bytes
   → write to `blake3/<contenthash>.wasm` (atomic: write to temp,
   fsync, rename) → write `uri_index/<urihash>.json` → load.

Pinned loads (`.load blake3:<hash>`) skip the URI layer entirely and
load from `blake3/<hash>.wasm`. If `<hash>` isn't already cached,
the load fails — pinned loads are explicit "I trust these bytes by
hash" semantics.

### Resolver registration

Two paths:

1. **Bootstrap.** A default resolver location lives in config — the
   host can preload it before the first `.load`. Initial default:
   look for `$CACHE_DIR/resolvers/http-resolver.wasm`, register it
   for `https`/`http` if present. Ships a hint to the user if not.
2. **Runtime.** A new `.register-resolver <scheme> <path>` CLI
   command (and the equivalent host API) loads a resolver as an
   extension and binds it to one scheme. `.unregister-resolver
   <scheme>` removes it.

### URI grammar

Three shapes accepted by `.load`:
- `<path>` — filesystem path (today's behavior; preserved)
- `<scheme>://<rest>` — dispatched to scheme's resolver
- `blake3:<hex>` — pinned content hash; resolved from cache only

`file:///abs/path` is treated as the filesystem path case — handled
by a built-in `file` resolver in the host, not a separate component
(no reason to round-trip bytes through a sandbox for a local read).

## Steps

### Step 1 — WIT: `resolver` interface + `resolving` world

In `sqlite-loader-wit/wit/`:

```wit
// add to guest.wit
interface resolver {
    /// Fetch the component bytes addressed by `uri`. The host
    /// hashes + caches whatever this returns. URI parsing is the
    /// resolver's responsibility — only the scheme determines
    /// which resolver gets the call.
    resolve: func(uri: string) -> result<list<u8>, string>;
}

// add to world.wit
world resolving {
    import types;
    import logging;
    import config;
    import http;

    export metadata;
    export resolver;
}
```

A resolver's manifest declares the `http` capability (or `state` if
it caches across loads internally). Resolvers don't export
`scalar-function` — they don't run inside SQL.

### Step 2 — CAS cache module in host

New `host/src/cache.rs`. Public surface:

```rust
pub struct Cache { root: PathBuf }
impl Cache {
    pub fn open(root: PathBuf) -> Result<Self>;     // mkdir -p, validate writeable
    pub fn lookup_by_uri(&self, uri: &str) -> Option<UriEntry>;
    pub fn lookup_by_hash(&self, hash: &str) -> Option<PathBuf>;
    pub fn put(&self, uri: &str, bytes: &[u8]) -> Result<String>;  // returns hash
    pub fn list_uris(&self) -> Vec<UriEntry>;       // for `.cache list`
    pub fn purge(&self) -> Result<usize>;           // for `.cache clear`
}

pub struct UriEntry { uri: String, hash: String, fetched_at: u64, ... }
```

Atomic writes via `tempfile::persist`. Dep: `blake3`, `tempfile`.

Default root resolution (in `sqlite-wasm-run`, not the lib):
1. `--cache-dir <path>` flag — explicit override
2. `$SQLITE_WASM_CACHE_DIR` env — secondary override
3. `$XDG_CACHE_HOME/sqlite-wasm/extensions`
4. `$HOME/.cache/sqlite-wasm/extensions`

### Step 3 — Resolver loading in host

New on `Host`:
- `register_resolver(&self, scheme: &str, path: PathBuf, policy: Policy)`
  — loads the path as a component via the `resolving` world, instantiates
  once to read manifest, stores the `LoadedExtension` keyed by `scheme`.
- `unregister_resolver(&self, scheme: &str)`
- `resolve_uri(&self, uri: &str) -> Result<Vec<u8>>`
  — parses scheme, looks up resolver, builds Store, calls
  `resolver.resolve(uri)`.
- `load_extension_from_uri(&self, uri: &str, policy: Policy) -> Result<String>`
  — orchestrates: cache lookup → resolve → cache write → existing
  `load_extension` against the cached path.

A fourth bindgen module `loaded_resolving` (world: `resolving`)
follows the `loaded_collating` pattern. The stateful linker already
provides `http` so we may be able to reuse it; if not, a dedicated
`make_loaded_resolving_linker` mirrors the existing helpers.

### Step 4 — http-resolver test extension

New crate in `sqlite-wasm-loader/runtimes/wasmtime/http-resolver/`,
built against `world: resolving`. Implementation:

```rust
impl ResolverGuest for HttpResolver {
    fn resolve(uri: String) -> Result<Vec<u8>, String> {
        let req = Request { method: Method::Get, url: uri, ... };
        let resp = http::handle(req).map_err(|e| format!("{e:?}"))?;
        if resp.status >= 200 && resp.status < 300 {
            Ok(resp.body)
        } else {
            Err(format!("HTTP {}", resp.status))
        }
    }
}
```

Manifest declares `Capability::Http`. The host's
`http_policy.allowed_hosts` gates which URLs the resolver can hit —
this is the *only* enforcement layer; the resolver itself does no
host validation. That delegation is the point.

### Step 5 — CLI surface

In `src/cli/sqlite_cli.c`:
- `.load <uri-or-path>` — extend to detect schemes and call a new
  in-WASM `extension-loader.load-from-uri(uri)` method (or extend
  `load-extension` to accept URIs and dispatch internally).
- `.register-resolver <scheme> <path>` — calls into a new WIT method
  `extension-loader.register-resolver(scheme, path, options)`.
- `.unregister-resolver <scheme>`
- `.resolvers` — list registered resolvers.
- `.cache list` / `.cache clear` / `.cache pin <uri>` (mark immutable).

New WIT methods on `extension-loader`:

```wit
register-resolver: func(scheme: string, path: string, opts: load-options)
    -> result<_, loader-error>;
unregister-resolver: func(scheme: string) -> result<_, loader-error>;
list-resolvers: func() -> list<tuple<string, string>>;  // (scheme, ext-name)
load-from-uri: func(uri: string, opts: load-options)
    -> result<manifest, loader-error>;
```

### Step 6 — Bootstrap

The host's `Host::new()` already takes no resolver config. Add
`Host::with_default_resolver(self, scheme: &str, path: PathBuf) -> Self`
so `sqlite-wasm-run` can preload the built http-resolver from the
cache dir if present. Document the install path in the CLI's
startup output (one line: `resolvers: https → http-resolver` or
`resolvers: none registered`).

### Step 7 — End-to-end validation

```
$ cp build/http-resolver.wasm ~/.cache/sqlite-wasm/extensions/resolvers/
$ sqlite-wasm-run build/sqlite-cli-demo.wasm
sqlite> .resolvers
https → http-resolver 0.1.0

sqlite> .load https://example.com/extensions/agg-extension.wasm
Loaded extension: agg-extension 0.1.0 (1 functions)
[cached at $CACHE/blake3/4f/8a/4f8a…wasm]

sqlite> .load https://example.com/extensions/agg-extension.wasm
Loaded extension: agg-extension 0.1.0 (cached, 1 functions)
[no http fetch]

sqlite> .load blake3:4f8a…wasm
Loaded extension: agg-extension 0.1.0 (from hash, 1 functions)

sqlite> .cache list
https://example.com/extensions/agg-extension.wasm → 4f8a… (12.4 KB, 30s ago)
```

A negative path: try to load from a host the resolver's policy
doesn't allow → `Err("http policy refused: example.com not in
allowed_hosts")` surfaced as a SQL error, never reaches the cache.

## Risks

- **Resolver authorship is the trust boundary.** A malicious
  resolver could ship bytes that don't match what `https://…`
  publicly serves. Mitigation: pinned-hash loads (`blake3:…`) for
  production; for ad-hoc loads users should pick resolvers they
  trust (same as they'd pick a TLS root store). Documenting this
  prominently in the CLI is part of the work.
- **wasi-http-shaped vs real wasi-http.** Our `http` interface is a
  flattened shape, not literal `wasi:http`. Resolvers can't use
  off-the-shelf HTTP client crates that target wasi:http; they have
  to call our `http.handle()`. Adapter glue is small but real.
  If/when we adopt real wasi-http, resolvers can switch with minor
  refactors.
- **Cache poisoning across users.** If two users share a cache
  (NFS-mounted, etc.), one can write a hash file the other reads.
  Mitigation: cache dir defaults under `$HOME`; documented as
  user-scoped. Multi-user installations need explicit per-user
  cache dirs.
- **HTTP idempotency / freshness.** v1 cache is "fetch once, never
  re-check". Add `etag` + `If-None-Match` later; for v1 users who
  want a fresh fetch run `.cache evict <uri>` or `.load --no-cache
  <uri>`.

## Dependency graph

```
Step 1 (WIT) ─┬─→ Step 3 (Host loading)
              └─→ Step 4 (http-resolver crate)
Step 2 (Cache) ──→ Step 3 (Host loading) ──→ Step 5 (CLI surface) ──→ Step 7 (E2E)
                                          └→ Step 6 (Bootstrap)
```

Reasonable order: 1, 2 (parallel) → 3 → 4 → 5, 6 (parallel) → 7.

## Out of scope

- OCI registry resolver (separate crate later — same pattern, no
  new architecture). Same for s3://, ipfs://, etc.
- Signature verification (sigstore / cosign). The CAS gives content
  identity but not authenticity-of-publisher. Layer signatures over
  the existing `blake3:<hash>` pin or on top of `uri_index` entries.
- Multi-resolver chains (try N resolvers per scheme). Scheme-keyed
  map is a strict 1:1; multi-resolver requires conflict-resolution
  policy.
- HTTP cache validators (etag / cache-control). v1 fetches once and
  trusts the cache until evict.
- Network transports beyond HTTP/HTTPS (gRPC, custom). Adding them
  is "write a new resolver" — no new core work.

## Branch strategy

One branch per major chunk:
- `feat/resolver-wit` — Step 1 only (sqlite-loader-wit + submodule bumps)
- `feat/cas-cache` — Step 2 (host-only, with unit tests)
- `feat/resolver-loading` — Step 3 + 6 (host bindgen + bootstrap)
- `feat/http-resolver` — Step 4 (extension crate)
- `feat/cli-uri-load` — Step 5 + 7 (CLI surface + e2e tests)
