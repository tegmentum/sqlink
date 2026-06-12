# sqlite-runtime endpoint protocol

The `compose:dynlink/endpoint.handle(method, payload)` shape is
intentionally opaque bytes both ways — the compose project says
the encoding is an application concern. This document fixes the
convention sqlite-wasm uses for the `sqlite-runtime` provider so
future providers (std-text, std-hashing, …) can adopt the same
shape.

## Envelope

**Encoding:** Canonical CBOR (RFC 8949 canonical form), matching
`sys:compose/canon-cbor`. Determinism guarantees:

- UTF-8 NFC strings
- Shortest integer encodings
- Definite-length containers
- Sorted map keys (lexicographic byte order)

**Method:** the `method` parameter to `endpoint.handle` is a
lower-kebab-case verb. Method names are stable; new verbs are
additive.

**Payload:** the CBOR encoding of a single value whose shape is
defined per-method below.

**Response:** the CBOR encoding of either a single value (method's
declared response type) or a structured error.

## Value type

The `value` type used throughout maps directly onto our existing
`SqlValue` WIT variant:

| CBOR | Rust | SqlValue |
|---|---|---|
| `null` | `()` | `Null` |
| integer (negative or unsigned) | `i64` | `Integer` |
| float | `f64` | `Real` |
| string | `String` | `Text` |
| byte string | `Vec<u8>` | `Blob` |

CBOR floats are always f64 in our profile (RFC 8949 §4.2.2.2
permits the encoder to choose; we always emit f64 for
determinism).

## Methods

### `query` — execute a row-returning statement

**Request:**

```cbor
{ "sql": text, "params": [value...] }
```

**Response:**

```cbor
{
  "cols":      [text...],            # column names in declaration order
  "rows":      [[value...]...],      # one inner array per row
  "changes":   u64,                  # rows changed by this stmt
  "last-rowid": i64                  # last_insert_rowid() snapshot
}
```

Errors: see "Error envelope" below.

### `query-scalar` — execute, return one value

**Request:** same as `query`.

**Response:** a single `value` (cell 0 of row 0). Errors if the
statement returned zero rows or more than one cell.

### `execute` — execute a write statement

**Request:** same as `query`.

**Response:**

```cbor
{ "changes": u64, "last-rowid": i64 }
```

### `execute-batch` — execute one or more statements

**Request:**

```cbor
{ "sql": text }
```

(no params — batch executes don't support parameters per SQLite's
contract)

**Response:**

```cbor
{ "changes": u64 }
```

### `prepare` — prepare a statement, get a handle

**Request:**

```cbor
{ "sql": text }
```

**Response:**

```cbor
{ "stmt-id": u64 }
```

The stmt-id is server-allocated and per-instance. Handles are
opaque to the guest; finalize releases them.

### `step` — advance a prepared statement, return next row

**Request:**

```cbor
{ "stmt-id": u64 }
```

**Response:**

```cbor
{
  "done": bool,                      # true if no more rows
  "row":  null | [value...]          # null when done = true
}
```

### `finalize` — release a prepared statement

**Request:**

```cbor
{ "stmt-id": u64 }
```

**Response:** CBOR `null`.

### `manifest` — introspect the provider

**Request:** `null` (empty payload acceptable too).

**Response:**

```cbor
{
  "name":    text,                   # "sqlite-runtime"
  "version": text,                   # provider version
  "methods": [text...]               # methods this provider supports
}
```

The method list lets clients discover capabilities without
parsing version strings.

## Error envelope

Errors come back through the `result<list<u8>, error>` return of
`endpoint.handle`, NOT as a CBOR payload. The `error` is the
`sys:compose/types/error` shape from the orchestration project:

```
record error {
  code:    error-code,
  message: string,
  context: option<string>,
}
```

For sqlite-runtime errors, the code is one of:

| code | when |
|---|---|
| `invalid-input` | malformed CBOR payload, unknown method |
| `internal-error` | rusqlite::Error::SqliteFailure |
| `not-implemented` | method exists in this doc but not yet wired |

The `message` carries the human-readable error; `context` carries
the SQLite extended result code as a string when applicable
(`Some("SQLITE_CONSTRAINT_PRIMARYKEY")` for primary-key conflicts,
etc.).

## "live" methods

The non-live triple (`query` / `execute` / `execute-batch`) all
target the **shared** cli rusqlite::Connection. The Fiji function
sees committed-from-its-own-statement-flow consistency, same as a
loaded extension's `spi.execute` today.

There is no `-live` method in this protocol. The provider already
runs against the cli's connection, so by definition the function
sees what the cli sees — there's no separate committed-snapshot
view to distinguish from. (Contrast with the `sqlite:extension/spi`
WIT, which has both `execute` (committed snapshot via separate
connection) and `execute-live` (would-be re-entry — see
SPI-LIVE.md).)

## Future providers

When `std-text`, `std-hashing`, `std-encoding` ship as compose
providers, they follow the same shape:

- Method names lower-kebab
- CBOR canonical envelope
- `manifest` introspection method

Their docs would live next to this one. The shared infrastructure
(error envelope, CBOR conventions, manifest shape) is documented
here as the precedent.

## Why CBOR rather than postcard / msgpack / JSON

- **Determinism.** The compose project defines a canonical CBOR
  profile we get for free.
- **Compactness.** Small payloads matter for high-frequency
  invocation through `endpoint.handle`.
- **Tagging.** CBOR tags let us extend the value type later
  (e.g., timestamps, UUIDs) without breaking the wire shape.
- **Schema-optional.** Unlike protobuf / Cap'n Proto, no
  `.proto` file ships with the wasm.

JSON was considered and rejected: floats lose precision in
round-trips, byte strings need base64, no integer/float distinction
at decode time.

## Conformance

The protocol is in scope of the conformance suite's CBOR vectors
when CP8 adds them. Until then, the test of record is CP4's unit
tests against each method.
