---
sidebar_position: 3
title: Prefixes
description: SPARQL-style namespacing for SQL functions so extensions can scale without bare-name collisions silently shadowing.
---

# Prefixes

As the extension catalog grows, name collisions between SQL functions
are inevitable. Two extensions might both register `concat`. SQLite's
default behaviour is silent last-wins shadowing — the second
registration replaces the first; callers can't tell which
implementation they hit.

Sqlink prefixes solve this by giving every extension a short alias +
a globally-unique expansion, similar to SPARQL `PREFIX foaf:
<http://xmlns.com/foaf/0.1/>` declarations.

## Strictly additive

The hard constraint: **existing SQL must not break**. Users today
write `SELECT uuid_v4()`; that has to keep working exactly the same
way after this feature lands.

| Scenario | Bare `name()` behavior |
|---|---|
| No collision | Works — single registered impl. |
| Collision, no pin | Works — last-registered wins (SQLite default); load-time warning logged. |
| Collision, `.prefix prefer name extA` | Works — pinned to extA regardless of load order. |
| `extA__name()` qualified | Always works for any registered extension. |

The feature is purely additive: bare-name dispatch is unchanged from
today's behaviour; qualified forms (`prefix__name`) are always
available for callers who want explicit dispatch.

## Separator: `__` (double underscore)

```
uuid_v4()            -- bare name
sqlink__uuid_v4()    -- qualified
```

Double-underscore was chosen because it's legal in unquoted SQLite
identifiers, visually distinct from natural single-underscore names
like `uuid_v4`, and ASCII-only. SPARQL's `:` would require quoting
every call site; `.` conflicts with SQL's `schema.function` syntax.

## Expansion is opaque

Each prefix has a short name + a required **expansion** string. The
expansion can be a URL, a Java-style namespace, a UUID, or any
opaque token. Sqlink doesn't validate the format. The expansion is
the canonical function identity; the short prefix is a per-database
alias.

```toml
[package.metadata.extension]
preferred-prefix = "foaf"
prefix-expansion = "http://xmlns.com/foaf/0.1/"
```

## Dot commands

```
.prefix add NAME EXPANSION [DESC]   -- register a new prefix
.prefix list                        -- name | expansion | description | last_used
.prefix functions NAME              -- functions registered under this expansion
.prefix expansion NAME              -- print just the expansion
.prefix rename OLD NEW              -- change short alias (function identity unchanged)
.prefix modify NAME DESC            -- update description
.prefix delete NAME                 -- remove alias
.prefix prefer NAME EXTENSION       -- pin bare-name dispatch on collision (live)
.prefix unprefer NAME               -- remove a pin (full revert next session)
.prefix conflicts                   -- diagnose bare-name ambiguities
.prefix verify                      -- registry summary
```

## Tables

Prefix registrations live in the user database under the `__sqlink_*`
convention (matching the cas-cache's `__cas_*` pattern). Same db,
travels with the file.

```
__sqlink_prefix (name PRIMARY KEY, expansion, description, created_at, last_used_at)
__sqlink_prefix_function (expansion, function_name, n_args, extension_name, registered_at)
__sqlink_prefix_pin (function_name, n_args, expansion, set_at)
```

## Deprecation window

In v1, extensions that don't declare `preferred-prefix` +
`prefix-expansion` in their manifest get a synthetic fallback:

```
prefix = <crate-name>
expansion = sqlink-internal://<crate-name>
```

A deprecation warning fires at load time. In v1.1 the fallback
becomes a hard error; before then, every in-tree extension gets a
real prefix + expansion assigned (a documented v1.1 migration sweep
in [PLAN-followups](/plans/PLAN-followups)).

See the [full plan](/plans/PLAN-prefixes) for the source-of-truth design.
