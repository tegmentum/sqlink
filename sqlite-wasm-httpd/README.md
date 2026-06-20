# sqlite-wasm-httpd

HTTP/HTTPS server that executes SQL against a sqlite database and
returns JSON. Sibling to `sqlite-wasm-run`: same `--db PATH`
contract, same sqlite defaults (cache_size=-262144,
temp_store=MEMORY, synchronous=NORMAL). Native binary, links
libsqlite3-sys directly  no wasm runtime in the hot path.

## Built-in endpoints

| Method | Path | Behaviour |
|---|---|---|
| GET | `/health` | 200 `ok` |
| POST | `/sql` | body is the SQL string; returns `{columns, rows, rowcount}` JSON |
| GET | `/sql?q=URL_ENCODED` | same, GET form |
| GET | `/tables` | JSON list of user-table names |
| GET | `/schema/{name}` | `pragma_table_info` JSON |

These take precedence over any db-driven route (see below)  the
sysadmin's SQL surface is never shadowed.

## Database-driven router

The premise: an HTTP route is a function from `(method, path,
query, body)` to a response. That's a SQL query. Or a static
constant. Or a wasm component  whichever the operator picks per
route.

```sql
CREATE TABLE routes (
    method   TEXT NOT NULL,         -- 'GET', 'POST', or '*'
    pattern  TEXT NOT NULL,         -- GLOB pattern: '/users/*', '/health'
    handler  TEXT NOT NULL,         -- what runs  see `kind` for shape
    kind     TEXT NOT NULL DEFAULT 'sql',
                                    -- 'sql' | 'static' | 'wasm'
    status   INTEGER DEFAULT 200,
    ctype    TEXT,                  -- default application/json
    priority INTEGER DEFAULT 0
);
```

### kind='sql'

`handler` is SQL. Bind on every request: `:method`, `:path`,
`:query` (raw string), `:body` (request body as text), `:remote`
(peer addr).

Result interpretation:
- **0 rows**  204 No Content
- **1 row, 1 column** that value IS the response body
- **1 row, named `body` / `status` / `ctype` columns** structured response
- **1 row, multiple columns**  JSON object of the row
- **>1 rows**  JSON array of row-objects

### kind='static'

`handler` IS the response body verbatim. No SQL roundtrip, no
parsing. Use for health probes, static config, simple constants.
`status` and `ctype` columns still apply.

```sql
INSERT INTO routes (method, pattern, handler, kind, ctype) VALUES
    ('GET', '/health', 'ok',          'static', 'text/plain'),
    ('GET', '/version', '{"v":"1"}',  'static', 'application/json');
```

### kind='wasm'

`handler` names a wasm component pre-loaded via `--load NAME=PATH`
at server start. Each request becomes a JSON blob the component
receives as its single argument:

```
{
  "method":  "POST",
  "path":    "/upload",
  "query":   "v=1" | null,
  "remote":  "10.0.0.1:55432",
  "body":    { "text": "..." } | { "bytes_hex": "deadbeef..." }
}
```

Components must target the `sqlite:wasm/language-runtime` WIT
world  i.e. export `runtime.execute(source-name: string,
source: string) -> result<string, string>`. The dispatcher passes
`"request.json"` as `source-name` and the JSON blob as `source`.
Return a JSON object `{ "status": 201, "body": "...", "ctype":
"text/plain" }` to override the row's defaults, or just return
the raw body string.

```
sqlite-wasm-httpd --db api.db --load upper=upper.component.wasm
```

```sql
INSERT INTO routes (method, pattern, handler, kind) VALUES
    ('POST', '/upper', 'upper', 'wasm');
```

Each request gets a fresh wasmtime Store: components are stateless
across requests, which matches HTTP's contract  persistent state
belongs in the database, not in the handler. Components share the
host's on-disk compile cache (`SQLITE_WASM_COMPILE_CACHE`), so
restart cost is bounded after the first run.

## Quickstart

```
$ sqlite-wasm-httpd --db /tmp/api.db --init-routes
INFO routes table `routes` ready
INFO http://127.0.0.1:8080  db=/tmp/api.db  POST /sql | GET /sql?q=...

$ curl http://localhost:8080/hello
{}

$ curl -X POST http://localhost:8080/sql -d \
    "INSERT INTO routes (method, pattern, handler) VALUES \
     ('POST', '/upper', 'SELECT upper(:body) AS body')"

$ curl -X POST http://localhost:8080/upper -d 'hello world'
HELLO WORLD

$ curl -X POST http://localhost:8080/sql -d \
    "INSERT INTO routes (method, pattern, handler, ctype) VALUES \
     ('GET', '/echo/*', 'SELECT :path AS body', 'text/plain')"

$ curl http://localhost:8080/echo/anywhere/you/want
/echo/anywhere/you/want
```

## TLS

Three modes, mutually exclusive:

```
# Plain HTTP (default)
sqlite-wasm-httpd

# HTTPS with a self-signed cert (handy for dev / smoke)
sqlite-wasm-httpd --tls-self-signed

# HTTPS with operator-supplied PEMs
sqlite-wasm-httpd --tls-cert server.crt --tls-key server.key
```

## Why not regex path patterns?

GLOB ships in sqlite; regex doesn't (without an extension). v1
keeps the surface minimal. A user who needs `/users/{id}`
extraction can use GLOB `/users/*` and have the handler parse
`:path` via SQL string-splitting  the boundary is the same as
any other layer that maps path  args.
