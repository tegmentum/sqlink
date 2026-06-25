---
sidebar_position: 4
title: Cas-cache
description: Content-addressed storage for extension blobs + bundle metadata. Lives at ~/.cache/sqlink/cas.sqlite.
---

# Cas-cache

The **cas-cache** is a content-addressed store for extension component
blobs and bundle metadata. It lives at `~/.cache/sqlink/cas.sqlite`
by default (override via `SqliteCasStore::default_external_path`).

## Schema

The cas-cache is itself a sqlite db with a small fixed schema:

```sql
-- v1 + v2 (cas-cache.md): artifact storage
CREATE TABLE __cas_artifact (...);     -- blob + blake3 hash + sha256 mirror
CREATE TABLE __cas_uri (...);          -- name  hash bindings
CREATE TABLE __cas_meta (...);         -- schema_version + bookkeeping

-- v3 (PLAN-bundles.md): bundle registry
CREATE TABLE __cas_bundle (...);
CREATE TABLE __cas_bundle_member (...);
CREATE TABLE __cas_bundle_binary (...);
```

Schema migrations are forward-only and idempotent (`CREATE TABLE
IF NOT EXISTS`); the version row is in `__cas_meta(key='schema_version')`.

## Identity model

Every artifact (extension blob) is identified by its **blake3 hash**
of the bytes, with a **sha256 mirror** column so callers that only
know sha-256 can still find it. Same bytes  same hash  same row 
no duplication.

URIs are loose handles: a single artifact can have many URIs pointing
at it (e.g., `extension:uuid` + `file:///path/to/uuid.wasm`). Eviction
drops the artifact only when its last URI is gone AND the bundle
registry doesn't reference it.

## Eviction

LRU-based, capped at `StoreConfig::max_bytes` (default 1 GiB).
`set_uri` / `resolve_uri` touch `last_used_at`. Bundles reference
their members so eviction won't drop a blob a saved bundle still
references.

## Where it gets used

- **`.load PATH`** — host hashes the bytes, looks up by hash; if
  found, reuses the existing artifact row; if not, `cas.put_bytes()`
  inserts it.
- **`.bundle save NAME`** — records the member tuples into
  `__cas_bundle_member`, referencing each member's existing artifact
  row by content_hash.
- **`.bundle build NAME`** — produced binary path is recorded in
  `__cas_bundle_binary` (one row per target triple).
- **`sqlink --bundle-load NAME`** — for each member, looks up the
  artifact via cas-cache, errors clearly with remediation hints if
  the blob isn't present.

## Browser parity

The cas-cache lives at the same shape in the browser (PLAN-browser-
runtime.md): a sqlite db in IndexedDB-backed storage. Same code path,
same schema, just a different storage location for `cas.sqlite`.
