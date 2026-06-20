# registry handler

Serves the sqlite-wasm extension registry over HTTP. Loaded as a
wasm component into `sqlite-wasm-httpd` (sibling to handlers/echo,
handlers/sql, handlers/markdown).

Two source files baked in at build time via `include_str!`:
- `registry/index.json`  shipped catalog (176 entries; generated
  from `provenance/extensions.db` by `provenance/build_registry.py`)
- `registry/candidates.json`  wishlist (51 entries; generated from
  the `plugin_candidate` table)

Rebuilding the handler picks up the latest registry state.

## Endpoints

| Method | Path | Returns |
|---|---|---|
| GET | `/` | the whole `registry/index.json` |
| GET | `/candidates` | the wishlist (planned ports) |
| GET | `/ecosystem` | merged shipped + planned with state field |
| GET | `/name/<name>` | one entry (shipped or candidate); 404 if absent |
| GET | `/search?q=<q>` | case-insensitive substring match on name + description |
| GET | `/tracks` | count by candidate track |
| GET | `/categories` | count by shipped category |
| GET | `/stats` | `{shipped: N, planned: N, total: N}` |

## Wiring

```
$ ./build.sh
wrote target/wasm32-wasip2/release/wasm_registry_handler.component.wasm

$ sqlite-wasm-httpd --db api.db --init-routes \
    --load registry=.../wasm_registry_handler.component.wasm
```

Then add routes via SQL:

```sql
INSERT INTO routes (method, pattern, handler, kind) VALUES
    ('GET', '/',           'registry', 'wasm'),
    ('GET', '/candidates', 'registry', 'wasm'),
    ('GET', '/ecosystem',  'registry', 'wasm'),
    ('GET', '/name/*',     'registry', 'wasm'),
    ('GET', '/search',     'registry', 'wasm'),
    ('GET', '/tracks',     'registry', 'wasm'),
    ('GET', '/categories', 'registry', 'wasm'),
    ('GET', '/stats',      'registry', 'wasm');
```

## Refresh cycle

The registry data is captured at component build time. To refresh:

```
$ make ext NAME=<anything>             # any catalog change triggers
                                       # scan + build_registry
$ ./sqlite-wasm-httpd/handlers/registry/build.sh
$ # restart sqlite-wasm-httpd
```

`build.sh` calls `provenance/build_registry.py` first so the
embedded registry is always current relative to the catalog.
