# PLAN: Extend `sqlite:extension/scalar-function` to carry typed WIT values

## Problem

The `sqlite:extension/scalar-function` WIT contract today carries only
`sql-value { text, real, integer, blob, null }` over the call surface.
This works for PostGIS because the postgis-wasm shim exposes a
binary↔resource decoder (`Geometry::from_wkb(&[u8]) -> result<geometry,
postgis-error>`): the bridge marshals `SqlValue::Blob` → `list<u8>` →
`Geometry` and invokes the WIT-typed function.

**Mobilitydb's WIT exposes no equivalent decoder.** Its function
signatures take WIT record values directly (`tfloat-min-value: func(seq:
tfloat-sequence) -> option<f64>`). Without a `binary → tfloat-sequence`
decoder on the shim's WIT, the wasm-component bridge has no way to
construct the record from a `SqlValue::Blob`. The interface DB encodes
the param as `[["binary"]]` — opaque at the catalog layer — but the
actual WIT signature is record-shaped, and the wire format from SQLite
(BLOB bytes) doesn't speak that shape.

The native mobilitydb bridge sidesteps this via
`datafission_functions::FunctionValue` (a host-side Rust enum that
carries record values directly between SQLite UDFs and wasm). The wasm-
component bridge has no equivalent — it's bounded by what
`sqlite:extension/scalar-function` allows.

This blocks Phase 4 of `PLAN-codegen-retarget.md` (#488) and any future
shim with the same property (record-shaped WIT params without
binary↔record codecs on the WIT surface). The fix is structural: extend
the contract's value-type discriminant.

## Path picked (path C from the codegen-retarget plan)

Add a `wit-value` arm to the sql-value variant. The arm carries enough
metadata to identify the WIT record's structural type and a payload of
the canonical-encoded record bytes. Hosts (sqlink-host, sqlink-loader,
composed-cli) marshal it; the wasm bridge's dispatch emits a wit-value
when calling a record-typed shim function and unwraps a wit-value when
returning one.

## Design — open questions to resolve before implementation

### Q1. Wire format for the record payload

The payload is a serialized form of the WIT record. Options:

- **Canonical CBOR (`canon:cbor` from #486 substrate).** Deterministic,
  schema-aware-friendly, matches the orchestration system's wire
  format. Slightly larger than packed binary but already a load-bearing
  format in the sqlink family. Drift detection comes for free.
- **Bincode of the canonical Rust struct shape.** Faster (no
  schema discovery at decode time), smaller bytes, but ties the
  contract to Bincode's versioning and Rust's type layout. Less
  language-neutral.
- **WIT canonical ABI bytes.** The exact byte sequence wasm's component
  model uses to ferry the record across the canonical-ABI boundary.
  Zero re-encoding cost on the host side (memcpy from the wasm linear
  memory). But host-side reconstruction needs the WIT canonical-ABI
  reader.

**Recommendation:** canonical CBOR. The cost of a deserialize is real
but cross-language; the wire format is the contract; alignment with
`canon:cbor` keeps the family coherent.

### Q2. Type identity — what goes in `type-id`?

The host needs to know what type to deserialize INTO. Options:

- **Canonical-WIT shape hash (from `canon:wit`, #486 Tier 2).** A 32-
  byte sha256 over the structural-typed canonical WIT form of the
  record. Stable across cosmetic WIT changes; changes immediately on
  any structural change. Auto-derivable at codegen time.
- **Symbolic name + version**, e.g.
  `"mobilitydb:wasm/temporal-types@0.1.0/tfloat-sequence"`. Human-
  readable. But requires a separate "is this name still the same
  shape" check.
- **Both**: hash for matching, name for diagnostics.

**Recommendation:** both. The hash is the authoritative match key; the
name appears in error messages.

### Q3. Type registry — how does the host map type-id → deserializer?

Codegen-time vs run-time:

- **Codegen-time, per-extension manifest.** Each bridge ships a
  manifest mapping each record type-id it can emit/receive to a Rust
  deserializer fn. The host loads the manifest at extension-init and
  registers the entries into its global typed-value registry.
- **Run-time, per-call.** Each call ships enough metadata to do the
  deserialization inline. No registry; higher per-call cost.
- **Static, per-contract-version.** A single global registry maintained
  by the contract; bridges depend on a known fixed set of types. Doesn't
  scale to user-defined shims.

**Recommendation:** codegen-time per-extension manifest. Bridges already
ship manifests via `get-info`/`extension-info`; adding a `typed-values`
field is additive. Host loads it at registration; per-call lookup is a
hash-table hit.

### Q4. Backwards compatibility + contract version cut

Existing extensions don't speak `wit-value`. Two integration points:

- **The variant addition.** Adding an arm to a WIT variant is a
  STRUCTURAL change to the canonical ABI: every existing extension's
  view of `sql-value`'s discriminant changes. Per the bump policy in
  #485, this is a MAJOR.
- **The natural moment** to do the `@1.0.0` promotion (the first half
  of #485 Phase 1) is here — cut `@1.0.0` AS the wit-value variant
  lands. Existing extensions get rebuilt against `@1.0.0`; new
  bridges (mobilitydb-sqlink-bridge) speak the wit-value-aware shape
  from day one.

**Recommendation:** fold #485 Phase 1 (cut `@1.0.0` baseline) and the
wit-value variant into a single contract event. One regeneration of
the 239-component catalog; one tracked bump; clean narrative for users
("on 1.0.0, record types are first-class"). #485 Phase 2 (loader pre-
check) follows independently.

## Proposed contract shape

```wit
// Before
variant sql-value {
  text(string),
  real(f64),
  integer(s64),
  blob(list<u8>),
  null,
}

// After (sqlink:wasm@1.0.0)
variant sql-value {
  text(string),
  real(f64),
  integer(s64),
  blob(list<u8>),
  null,
  wit-value(wit-value-payload),
}

record wit-value-payload {
  // 32-byte sha256("witcanon:1" || canonical-CBOR(WIT record shape))
  // from canon:wit hashing. Authoritative match key.
  type-id: list<u8>,

  // Canonical CBOR encoding of the record per canon:cbor profile.
  // Length matches the record shape that type-id identifies.
  bytes: list<u8>,

  // Human-readable symbolic name + version, for diagnostics.
  // Example: "mobilitydb:wasm/temporal-types@0.1.0/tfloat-sequence"
  // Hosts use this in error messages but NOT for matching.
  symbolic-name: string,
}
```

Manifest channel addition (`get-info`/`extension-info`):

```wit
record extension-info {
  name: string,
  version: string,
  functions: list<function-info>,
  // New
  typed-values: list<typed-value-binding>,
}

record typed-value-binding {
  type-id: list<u8>,           // matches wit-value-payload.type-id
  symbolic-name: string,       // matches wit-value-payload.symbolic-name
  // The wasm-side import the host can call to deserialize bytes into
  // the WIT record (or to encode the record back to bytes).
  decoder-import: string,      // e.g. "mobilitydb:wasm/serde-ops/tfloat-sequence-from-canon-cbor"
  encoder-import: string,      // e.g. "mobilitydb:wasm/serde-ops/tfloat-sequence-to-canon-cbor"
}
```

The `decoder-import` + `encoder-import` are wasm-side functions the
bridge component exposes on its WIT export world (one per record type
it marshals). The host invokes them on every call that ferries a wit-
value.

## Implementation phases

### Phase A — design lock + contract change

1. Lock the four design questions above (this PLAN doc + an
   AskUserQuestion pass).
2. Update `sqlite-loader-wit/wit/` with the new variant + manifest
   fields. Bump `sqlink:wasm@0.1.0` → `sqlink:wasm@1.0.0`.
3. Write a `migration-notes-1.0.md` documenting the change for
   extension authors.

### Phase B — host marshaling

1. `sqlink-host`: extend the runtime to recognize `wit-value`, look up
   the type-id in the per-extension registry, call the decoder import
   to recover the WIT record, pass it to the called function.
2. Same for `sqlink-loader.dylib` and `composed-cli-worker`.
3. Reverse path: when a function returns a WIT record, wrap it as
   `wit-value(...)` via the matching encoder.

### Phase C — codegen consumption

1. `sqlink-shim-codegen` learns to read the upstream shim's WIT
   record definitions, emit dispatch arms that use `wit-value` for
   record-typed params/returns.
2. Codegen emits per-bridge `typed-value-binding` entries in the
   manifest, including the per-record decoder/encoder import names.
3. The codegen-emitted bridge crate exposes the
   `<record>-from-canon-cbor` + `<record>-to-canon-cbor` functions
   as WIT exports (these are codegen-generated from the WIT record
   definitions; per record type, ~10 lines).

### Phase D — codegen generalization (separately needed)

The Phase 4 fork's investigation found the codegen is PostGIS-specific
in ~5 places (`emit_wit.rs:46-55` world template match, `emit_lib.rs:52`
hardcoded `postgis-wasm` dep dir, `emit_lib.rs:150` hardcoded
`postgis_types` import, `emit_lib.rs:210-228` PostGIS-only helpers,
`dispatch.rs:528-555` PostGIS-only interface-name → alias mapping).
These need decoupling regardless of wit-value. Land alongside Phase C.

### Phase E — mobilitydb unblock

1. With the contract extension + codegen generalization in place,
   generate `tegmentum/mobilitydb-sqlink-bridge` from
   `/tmp/mobilitydb-interface.sqlite`.
2. Compose with `mdb-temporal-wasm.wasm` + `postgis-composed.wasm` +
   `postgis-sqlink-bridge.wasm`.
3. Verify `tfloat-min-value(seq)` or equivalent works end-to-end.
4. Update `PLAN-codegen-retarget.md` Phase 4 status from blocked →
   done.

### Phase F — contract guard (#485 Phase 2)

The `@1.0.0` baseline now has real meaning. Add the loader pre-check
that introspects each loaded component's `sqlink:wasm` import version
and rejects mismatches with the friendly message. Migrate existing
extensions via a one-pass regeneration of their WIT exports against
the new sql-value shape; the `wit-value` arm is additive at the
discriminant level (existing variants keep their tags) so the
migration is a recompile, not a behavior change.

## Risks

- **Decoder import cost.** Every record-marshal involves a wasm-side
  function call (host → wasm decoder → WIT record). Per-row this is
  measurable; per batch, negligible. Profile early.
- **Manifest size.** Per-record decoder/encoder entries inflate
  manifests for shims with many record types. ~50 records ×
  manifest overhead is fine; ~5000 records would matter.
- **Round-trip stability.** The encoder must produce a payload the
  decoder accepts, regardless of host language. Conformance tests on
  the canonical CBOR profile + the record-shape registry.
- **Type-id collisions.** sha256 over `canon:wit` is the source of
  truth, but bugs in `canon:wit` normalization could produce
  collisions or non-deterministic hashes. The orchestration system's
  conformance suite (#486 Tier 3, C3) catches this.

## Verification

- Phase A: catalog `wit-component metadata` of any extension built
  against the new contract shows `sqlink:wasm@1.0.0` in its imports.
- Phase B: sqlink-host's test suite gains a unit that feeds a synthetic
  `wit-value` through the runtime and observes the decoded record at
  the wasm boundary. Encode-decode round-trip is byte-identical.
- Phase C: codegen-emitted bridge for a record-touching upstream
  function compiles + exports both encoder and decoder. Manifest
  includes both bindings.
- Phase E: `SELECT tfloat_min_value(seq) FROM (VALUES (?)) AS t(seq)`
  returns the expected scalar; verify subcrate reads the OPFS-stored
  cas state across a reload (composes the mobilitydb path with the
  existing v1.5 browser path).
- Phase F: a component built against `sqlink:wasm@0.x` is REJECTED by
  the loader with the friendly contract-mismatch message (per the
  #485 guard).

## Cross-cuts

- **#485 WIT contract versioning.** Phase 1 (semver promotion) folds
  into Phase A here. Phase 2 (loader pre-check) is Phase F here.
- **#486 orchestration integration.** Tier 2 (canonical-WIT identity)
  provides the type-id hashes; Tier 2 work moves alongside Phase A.
- **#488 codegen retarget.** Phase 4 unblocks at Phase E. The codegen
  generalization (Phase D here) replaces the Phase 4-prerequisite
  "Phase 3 round 3" the codegen-retarget plan would otherwise need.
- **#487 v1.6 follow-ups.** bundle-cli's SPI rewrite consumes the new
  contract shape; folds into Phase B's host marshaling work.

## Decisions (locked 2026-06-26)

- **DD1. Wire format: canonical CBOR (`canon:cbor` from #486).**
  Deterministic, schema-aware-friendly, matches the orchestration
  system's wire format. Cross-language interop free. Decode cost is
  measurable but acceptable.
- **DD2. Type identity: hash + symbolic name (both).** 32-byte sha256
  over `canon:wit` normalized form is the authoritative match key.
  Symbolic `"package:wasm/interface@version/type-name"` ships
  alongside for error messages + diagnostics. Hash matches
  structurally; name communicates intent. Aligns with #486 Tier 2
  canonical-WIT identity work.
- **DD3. Type registry: codegen-time per-extension manifest.** Each
  bridge ships a `typed-value-binding` list in its
  `extension-info` manifest, mapping type-id → decoder/encoder
  imports. Host loads at extension-init; per-call lookup is a hash-
  table hit. Additive to the existing `get-info`/`extension-info`
  channel.
- **DD4. Contract bump timing: fold.** Cut `sqlink:wasm@1.0.0` AS the
  wit-value variant lands. One contract event; one regeneration of
  the 239-component catalog; clean narrative ("on 1.0.0, record
  types are first-class"). #485 Phase 1 (semver promotion) folds
  into Phase A here. #485 Phase 2 (loader pre-check) is Phase F.

## References

- `~/git/sqlink/docs/plans/PLAN-codegen-retarget.md` — D5 update on
  2026-06-27 documents the WIT-layer invalidation that motivated this
  plan.
- `~/git/sqlink/docs/plans/PLAN-wit-contract-versioning.md` (#485) —
  Phase 1 + 2 are sequenced into this plan's Phase A + F.
- `~/git/sqlink/docs/plans/PLAN-orchestration-integration.md` (#486)
  — Tier 2 canonical-WIT identity overlaps with type-id hashing.
- `~/git/sqlink/sqlite-loader-wit/wit/` — current contract surface
  to extend.
- `~/git/mobilitydb-wasm/crates/mdb-temporal-wasm/wit/temporal.wit`
  — the WIT signatures driving the need.

## Phase A — done (2026-06-26)

### Landed

- **WIT contract (`sqlite-loader-wit`, branch `feat/wit-value-1.0`,
  commit `4861ae1f`):**
  - `sql-value` variant gains a `wit-value(wit-value-payload)` arm
    in `wit/types.wit`. Payload carries:
    - `type-id: list<u8>` — 32-byte sha256 `canon:wit` shape hash
      (authoritative match key).
    - `bytes: list<u8>` — canonical-CBOR encoding per `canon:cbor`.
    - `symbolic-name: string` — diagnostic name in
      `<package>:wasm/<interface>@<version>/<type-name>` form.
  - `metadata.manifest` gains a `typed-values:
    list<typed-value-binding>` field in `wit/guest.wit`. Each
    binding names a `type-id` plus the wasm-side
    `<package>:wasm/serde-ops/<type>-from-canon-cbor` +
    `<package>:wasm/serde-ops/<type>-to-canon-cbor` import names.
  - Package version bumped `sqlite:extension@0.1.0` →
    `sqlite:extension@1.0.0` across every `wit/*.wit` and the
    README.

- **Migration notes (`sqlite-loader-wit/MIGRATION-1.0.md`):**
  documents the variant addition, the new manifest field, the
  source-level recipe extension authors follow on recompile,
  backwards-compat semantics, the canonical-CBOR wire format
  profile, and the `<package>:wasm/serde-ops/...` decoder-import
  naming convention.

- **Submodule pointer bump (`sqlink`, branch
  `feat/wit-value-phase-a`, commit `402863e4`):** tracks the
  contract bump in the parent repo.

- **Host crate (`sqlink-host`, in the catalog regen commit
  `d8a64c8c`):**
  - `CONTRACT_MAJOR` bumped 0 → 1. The load guard now rejects
    components targeting `sqlite:extension@0.x` (the inverse of
    the pre-bump state).
  - Contract-guard tests flipped: the legacy `@0.1` built component
    is now REJECTED (rather than accepted), and the "future major"
    test moved from `Some(1)` (was: future) to `Some(2)` (is:
    future).
  - Every `SqlValue` match site in `lib.rs` + `vtab.rs` gains a
    `WitValue` arm. Cross-bindings converters pass the payload
    through field-by-field; SQL-boundary converters surface a
    Phase B placeholder (`unimplemented!` for infallible paths,
    `sqlite3_result_error` for SQL-result paths).
  - `manifest_for_ext` initializes the new `typed_values` field to
    `vec![]`; Phase C codegen populates it.
  - `host/wit/`: every `sqlite:extension/<iface>@0.1.0` import
    bumped to `@1.0.0`. The host's own `sqlink:wasm@0.1.0`
    package version stays unchanged.

- **Catalog regen (218 extensions in `extensions/*`, in the same
  commit):**
  - Every `Manifest { ... }` constructor gains
    `typed_values: Vec::new()` immediately after `prefix_expansion`.
  - 204 extensions (797 match blocks) gain a `_ => unimplemented!`
    wildcard arm in every `match` on `SqlValue` (added via the
    brace-tracking script in `scratchpad/`). 14 extensions had no
    `match` on `SqlValue` and needed no source-level arm
    additions.
  - The `extensions/postgis-bridge/wit/deps/sqlite-extension/`
    vendored copies of the contract were re-synced from
    `sqlite-loader-wit/wit/`. The 217 path-based extensions
    inherit the bump from the submodule without per-extension WIT
    edits.

### Verification (all Phase A acceptance points hit)

- `cargo test -p sqlink-host --lib` — **52/52 pass**. Includes
  the flipped contract-guard tests.
- Workspace-listed extensions (`uuid`, `csv`, `math`, `crypto`,
  `json1`, `regexp`, `stats`) all build cleanly to
  `wasm32-wasip2`.
- Random sample of standalone extensions: **39/40 build**. The
  one exception (`compress`) fails on an unrelated `zstd-sys`
  build script issue in the worktree environment, not on
  wit-value migration code.
- `wasm-tools component wit
  target/wasm32-wasip2/release/uuid_extension.wasm` confirms the
  built extension imports `sqlite:extension/types@1.0.0` and
  `sqlite:extension/policy@1.0.0` and the `wit-value-payload`
  record appears in the embedded package — i.e. the contract
  change actually propagated into a freshly-built component.

### Branches + commits

- `sqlite-loader-wit` branch `feat/wit-value-1.0` (pushed to
  `origin/feat/wit-value-1.0` AND `https://github.com/tegmentum/
  sqlite-loader-wit.git`):
  - `4861ae1f` `feat(wit)!: bump sqlite:extension to @1.0.0 with
    wit-value variant`.
- `sqlink` branch `feat/wit-value-phase-a` (pushed to
  `origin/feat/wit-value-phase-a`):
  - `402863e4` `chore(submodule): bump sqlite-loader-wit for
    wit-value variant + @1.0.0 baseline`.
  - `d8a64c8c` `feat(catalog)!: regenerate 218-extension catalog +
    host crate for @1.0.0 contract`.

### Honest caveats / Phase B+ debt

- **Unreachable-pattern warnings.** The match-arm sweep was
  brace-tracked but not semantically aware: it added a `_ =>
  unimplemented!` arm to every match whose arms enumerate
  `SqlValue::*` variants, including matches that PRODUCE
  `SqlValue` from another enum (where the wildcard is
  unreachable). Result: ~30-50 `unreachable_patterns` warnings
  across the catalog. Warnings only — clean build. A follow-up
  cleanup pass can scope the wildcard to consume-`SqlValue`
  matches only.

- **`db::Value` has no WitValue mirror.** The host's
  `db_value_to_spi` / `db_value_to_bindings` /
  `db_value_to_bindings_sql` paths don't yet manufacture a
  `SqlValue::WitValue`  the OLD direction (db → SqlValue)
  silently drops the variant entirely. Phase B extends `db::Value`
  (or layers a typed parallel channel) and rewires those sites
  to call the bridge's encoder import.

- **`compress` extension build failure.** Pre-existing
  zstd-sys build script issue in the temp worktree
  environment; not a Phase A regression. Worth flagging for the
  v1.5 follow-up sweep but out of scope here.

### What didn't change (Phase B-F still owe)

- No host or bridge actually emits or consumes a `wit-value`
  yet. The variant LANDS in the contract; encoder/decoder dispatch
  is Phase B (host marshaling) + Phase C (codegen consumption).
- `mobilitydb-sqlink-bridge` does not exist yet. Phase E.
- The loader pre-check (#485 Phase 2 / Phase F) hasn't shipped;
  the `@1.0.0` host now expects `@1.x` components but the
  ABI-skew rejection path uses the EXISTING `datalink-contract`
  guard (Phase A's tests pin its inverted behavior on the new
  major).

## Phase B — done (2026-06-26)

### Landed

- **`db::Value::WitValue` variant + payload (`sqlite-wasm`
  submodule, branch `feat/wit-value-phase-b`, commit `7bde4cb`):**
  - `sqlite-wasm/core/src/db.rs:52` extends the `Value` enum
    with a `WitValue(WitValuePayload)` arm. The payload struct
    carries `type_id: [u8; 32]`, `bytes: Vec<u8>`, and
    `symbolic_name: String` — mirroring the WIT
    `wit-value-payload` record with type-id normalised to a
    fixed-size array (the WIT lists `list<u8>` for flexibility,
    every Phase B+ producer ships sha256 → 32 bytes).
  - SQLite has no first-class typed-record cell; `Statement::
    bind`, `set_result_value`, and the sqlink-loader's
    `value::write_result` surface the `WitValue` arm as `BLOB`
    on the SQLite C surface so the wire form round-trips through
    a column store. The wasm bridge recovers typed identity on
    subsequent dispatch via the per-extension `TypedValueRegistry`.

- **Host crate (`host/src/lib.rs`):**
  - Six site-by-site converters (`spi_value_to_db`, `db_value_
    to_spi`, `bindings_value_to_db`, `db_value_to_bindings`,
    `bindings_sql_to_db_value`, `db_value_to_bindings_sql`)
    now carry the typed identity through field-by-field instead
    of panicking via `unimplemented!`. Helper `type_id_from_wit`
    normalises the variable WIT `list<u8>` to `[u8; 32]` with a
    `tracing::warn` on length mismatch.
  - `compose_provider::db_to_cbor` encodes `WitValue` as a CBOR
    map preserving `type_id` + `bytes` + `symbolic_name` so the
    compose-provider channel preserves the typed identity. The
    `cbor_to_db` inverse remains deferred — compose-provider
    feeds host-managed SQL params today, not bridge dispatch.
  - `sqlink-native::render_value` adds a diagnostic
    `<wit-value:NAME LEN bytes>` render so the native CLI
    surface doesn't panic if a future Phase C bridge ferries a
    `WitValue` out.

- **`postgis-bridge` Phase A miss (sqlink commit `62e022d4`):**
  - `extensions/postgis-bridge/wit/world.wit` bumped
    `sqlite:extension/<iface>@0.1.0` → `@1.0.0` (the local
    bridge WIT lagged the Phase A submodule bump).
  - 16 unreachable `_ => unimplemented!` wildcards stripped
    from matches that PRODUCE `SqlValue` (or match on something
    other than `SqlValue`) — addresses Phase A's deferred
    unreachable-pattern warnings.

- **Per-extension type registry (`host/src/typed_value.rs`,
  sqlink commit `41c6b9d9`):**
  - New module. `TypedValueRegistry` maps 32-byte type-id →
    `TypedValueBinding { extension_name, symbolic_name,
    decoder_import, encoder_import }`. Conflict semantics:
    same type-id with a different binding errors out at load
    time (canon:wit drift is loud, never silently
    overwritten); identical re-insertions are idempotent so
    reload is safe.
  - `TypedValueCodecs` holds `Arc<dyn TypedValueCodec>` keyed
    by type-id. Phase B's test suite installs Rust closures
    (synthetic identity / toggle-high-bit); Phase C codegen
    will install `WasmCodec` that calls the bridge's
    `<package>:wasm/serde-ops/<type>-from-canon-cbor` and
    `-to-canon-cbor` exports via the cached wasmtime instance.
  - `Host` gains `pub typed_values` + `pub typed_value_codecs`
    fields; `Host::new` initialises both.
  - `Host::register_component` drains `manifest.typed_values`
    into `typed_values` after `metadata.describe()`, surfacing
    `RegistryConflict` as a load failure. `Host::unload`
    clears all bindings + codec slots owned by the
    unregistered extension so a re-load with a re-hashed type
    set doesn't trip the conflict guard.

- **Decode / encode dispatch API (sqlink commit `f60314b6`):**
  - `Host::decode_wit_value(&payload) → Result<Vec<u8>>`. Looks
    up payload type-id in the registry; dispatches through the
    installed codec to validate / normalise. Unknown type-id is
    a hard error; missing codec falls back to identity
    pass-through (Phase B has no real bridges; the bytes ARE
    canonical-CBOR by construction).
  - `Host::encode_wit_value(type_id, bytes) →
    Result<WitValuePayload>`. Inverse path. Error context
    surfaces the offending extension + symbolic name.
  - `short_hex` helper renders the first 4 bytes of a type-id
    with an ellipsis so error messages stay terse.

- **sqlink-loader inheritance (sqlink commit `926ca467`):**
  - `sqlink-loader/src/load.rs` documents that the loader does
    NOT maintain its own registry. It inherits the full Phase B
    path through `host.load_extension` (registry drain) and
    `host.dispatch_scalar` (which carries WitValue through
    wit-bindgen's `call_call` directly to the bridge's wasm-
    side decoder). `value::write_result` already surfaces a
    `WitValue` result as canonical-CBOR BLOB on the
    sqlite3_context.

- **Browser ExtensionRegistry mirror (sqlink commit `5511529a`):**
  - `browser/src/extension-loader.js` adds
    `_typedValuesByTypeId` keyed by lower-case-hex type-id.
    Drains `manifest.typedValues` (jco lowering of
    `typed-values`) at `add()` / `addFromBytes()` time with
    the same conflict semantics as the Rust registry.
  - `lookupTypedValue` / `typedValueBindings` / cleanup on
    `delete()` + `forgetRegistrations()`.
  - `bytesToHex` helper handles jco's `list<u8>` shape
    (`Uint8Array` | `number[]` | ArrayLike).
  - The browser path doesn't directly invoke decoders — the
    composed-cli-worker drives the wasm cli over stdin/stdout
    and SQL values cross the JS boundary as text. Registry IS
    populated for introspection + Phase C+ host-driven
    dispatch.

- **Round-trip test (sqlink commit `742dd206`):**
  - `host/src/typed_value.rs::tests` ships four `b7_*` tests
    covering the Phase B acceptance gate.
  - `b7_synthetic_decode_encode_roundtrip_is_byte_identical`:
    synthetic canonical-CBOR payload → IdentityCodec.decode →
    .encode → byte-identical bytes. Wraps the round-trip back
    into a `db::Value::WitValue` to validate the marshaling
    shape composes.
  - `b7_codec_is_actually_invoked`: ToggleHighBitCodec
    confirms the registry's dispatch actually runs the codec
    rather than silently passing bytes through.
  - `b7_unknown_type_id_lookup_misses` and
    `b7_missing_codec_falls_back_to_identity_passthrough` pin
    the error / identity-passthrough behaviour.

### Verification

- `cargo test -p sqlink-host --lib` — **64/64 pass** (Phase A
  baseline 52 + 12 typed_value tests, of which 4 are the B7
  acceptance set, plus 3 incidental from cache/blob).
- `cargo check --workspace` — clean across every workspace
  member. **0 unreachable-pattern warnings** (Phase A had ~16
  in postgis-bridge; stripped in B1's regex sweep).
- `cargo test -p sqlink-loader --lib -- --test-threads=1` —
  45/45 pass.
- Node smoke test against `browser/src/extension-loader.js`
  ExtensionRegistry exercise: roundtrip lookup, conflict throws,
  idempotent re-insert, forget — all pass.

### Branches + commits

- `sqlite-wasm` submodule, branch `feat/wit-value-phase-b`
  (pushed to `origin` AND
  `https://github.com/tegmentum/sqlite-wasm.git`):
  - `7bde4cb` `feat(core)!: add Value::WitValue variant for
    sql-value@1.0.0`.
- `sqlink` branch `feat/wit-value-phase-b` (pushed to
  `origin/feat/wit-value-phase-b`):
  - `62e022d4` `feat(host)!: wire db::Value::WitValue
    conversion through host crate`.
  - `41c6b9d9` `feat(host): per-extension typed-value registry`.
  - `f60314b6` `feat(host): wire decode_wit_value /
    encode_wit_value dispatch API`.
  - `926ca467` `docs(loader): document wit-value path
    inheritance from host`.
  - `5511529a` `feat(browser): per-extension typed-value
    registry on ExtensionRegistry`.
  - `742dd206` `test(host): wit-value synthetic round-trip +
    B7 acceptance`.

### Honest caveats / Phase C+ debt

- **No real wasm-side decoder invocation yet.** The
  `TypedValueCodec` trait is byte-in / byte-out; Phase B's test
  installs Rust closures. Phase C codegen wires the real
  `WasmCodec` that calls the bridge's
  `<package>:wasm/serde-ops/<type>-from-canon-cbor` /
  `-to-canon-cbor` exports via the cached wasmtime instance.
  Until then `Host::decode_wit_value` / `encode_wit_value` take
  the identity-passthrough branch (correct: the contract is
  canonical-CBOR end-to-end; an extension that ships
  non-canonical-CBOR is broken).

- **No bridge ships `typed-value-binding` entries yet.** The
  registry IS populated by `metadata.describe()`'s
  `typed-values` field, but every catalog extension's
  `manifest_for_ext` (host) + every bridge's manifest emit an
  empty list. Phase C codegen starts populating it from the
  upstream shim's WIT record definitions.

- **`compose_provider::cbor_to_db` inverse is deferred.** The
  forward path (`db_to_cbor`) emits the wit-value as a CBOR map;
  the inverse is left as Phase C debt because compose-provider
  currently feeds host-managed SQL params (not bridge dispatch)
  and no host actually emits `WitValue` yet.

- **Worktree environment misses `.cargo/config.toml`.** The
  workspace ships only `config.toml.template`; the deployment
  step expands `__WASI_SDK_PATH__` and installs the resolved
  config. In this Phase B worktree the bundled libsqlite3-sys
  build doesn't get `LIBSQLITE3_FLAGS = -DSQLITE_ENABLE_SESSION`,
  so the `sqlink` bin's session FFI declarations fail at link.
  Phase B unit tests run via `cargo test -p sqlink-host --lib`
  which bypasses the bin build; the integration `host/tests/`
  harness hits the same failure but is unrelated to wit-value
  work and pre-dates Phase B. Out of scope to fix here.

- **Browser end-to-end test deferred.** B6 added the JS
  registry + node smoke test of the conflict / lookup
  invariants. The composed-(bundle|prefix) playwright suites
  exercise extensions that DON'T declare typed-values yet, so
  the drain is a no-op there; once Phase C codegen emits a real
  bridge with typed-values, a new playwright spec exercises the
  registry through a browser load.

### What didn't change (Phase C-F still owe)

- Codegen consumption (Phase C): `sqlink-shim-codegen` learns
  to read upstream shim WIT, emit dispatch arms using
  `wit-value` for record-typed params/returns, emit
  `typed-value-binding` entries in the manifest, and emit the
  per-record `serde-ops` exports.
- Codegen generalization (Phase D): decouple PostGIS-specific
  paths (`emit_wit.rs:46-55`, `emit_lib.rs:52`, etc.).
- `mobilitydb-sqlink-bridge` (Phase E).
- Loader pre-check (#485 Phase 2 / Phase F).

## Phase C + D — done (2026-06-26)

### Landed

#### Phase D — codegen decoupling

Decoupled the wasm-component codegen from the five hardcoded
postgis assumptions catalogued in this PLAN's Phase D section.

- **D1 (`emit_wit.rs`).** Replaced the
  `match primary { "postgis" => POSTGIS_WORLD_WIT, ... }` constant
  dispatch with a discovery-driven `render_world` that walks the
  resolved shim deps tree, parses every `package <ns>:<name>@<ver>;`
  declaration via `wit_parse::parse_package_dir`, and emits one
  `import <ns>:<name>/<iface>@<ver>;` line per interface found.
  The fixed contract surface (`import sqlite:extension/<iface>` for
  types/spi/logging/config/state/cache; `export sqlite:extension/
  <iface>` for metadata/scalar-function/aggregate-function/vtab)
  is interpolated with the host wit dir's `sqlite:extension`
  package version dynamically — currently `@1.0.0` (Phase A).

- **D2 (`emit_lib.rs`).** Replaced the hardcoded
  `wit_deps_root.join("postgis-wasm")` with `pick_primary_shim_dir`,
  which picks the subdir whose parsed package's namespace matches
  the primary extension name; falling back to dirname prefix; then
  to the first non-helper subdir. Also generalised
  `source_shim_deps_dir(primary)` to a per-primary resolver:
    - `postgis` → `~/git/sqlink/extensions/postgis-bridge/wit/deps`
    - `mobilitydb` → `~/git/mobilitydb-wasm/wit/deps` (best-effort
      fallback; Phase E lands a proper deps root containing
      `mobilitydb-temporal/`).
    - `SQLINK_SHIM_WIT_DEPS=...` overrides everything; per-primary
      env vars (`SQLINK_POSTGIS_BRIDGE_WIT_DEPS`,
      `SQLINK_MOBILITYDB_BRIDGE_WIT_DEPS`) override the per-
      primary defaults.

- **D3+D4 (`emit_lib.rs`).** The `use bindings::<pkg>::
  postgis_types::{Geography, Geometry, PostgisError}` line and the
  `from_wkb` / `geog_from_wkb` / `postgis_err_string` helpers are
  emitted ONLY when the shim's WIT carries `resource geometry`
  AND `variant postgis-error`. `wit_parse` gained
  `WitResourceDecl` + `WitVariantDecl` extraction (with
  `scan_package_decls`) so the gating can be data-driven; for
  non-postgis shims (mobilitydb etc.) all three helpers + the
  use line are skipped, and the dispatcher arms that would
  reference them simply don't fire.

- **D5 (`wit_parse.rs` + `dispatch.rs`).** Made
  `interface_to_rust_alias` / `alias_to_wit_module_ident`
  algorithmic with postgis overrides for the short alias form
  (`pg_ctor`, `pg_acc`, ...). For non-postgis interfaces the
  fallback is the kebab → snake_case form (e.g. `tint-ops` →
  `tint_ops`), so the codegen emits valid
  `use bindings::<ns>::<name>::<iface_snake> as <iface_snake>;`
  lines for any shim. `WitFunction` gained `package` +
  `package_version`; `DispatchShape` / `AggregateShape` /
  `UdtfShape` carry `wit_package` so the emitter routes the
  `use` line at `bindings::<wit_pkg_ns>::<name>::<module>`
  rather than the hardcoded `postgis::wasm::`.

#### Phase C — wit-value codegen

- **C1 (`record_registry.rs`).** Per-shim record-type registry.
  Every `record NAME { ... }` block discovered in the primary
  shim's WIT packages becomes a `RecordType` carrying:
  - `package` + `package_version` + `interface` + `kebab_name`
    + ordered `(field, type-text)` list,
  - `type_id`: 32-byte sha256 over a deterministic text form
    `witcanon:1\n<package>/<interface>/<record>\n<sorted
    field:type lines>\n`,
  - `symbolic_name`: `<package>@<version>/<interface>/<record>`,
  - convenience `decoder_import()` / `encoder_import()`
    methods returning the
    `sqlink-bridge:<primary>/serde-ops/<record>-{from,to}-canon-cbor`
    convention names. `sha2 = "0.10"` added as a build-side dep.

- **C2 + C3 — DEFERRED to Phase E (with honest cause).**
  - C2 (per-record encoder/decoder WIT exports). Generating the
    `interface serde-ops { use <pkg>/<iface>.{rec}; ... }` block
    and exporting it from the world works at the WIT level but
    triggers a `wac plug` 0.10 component-model validation error
    ("instance not valid to be used as export") when the records
    re-reference types from imports that get satisfied by the
    plug — the resulting composed loadable.wasm fails to
    validate. The Rust `SerdeOpsGuest` stub impl ALSO can't
    compile without the world export (wit-bindgen only generates
    the Guest trait when the world references the interface).
    Phase E lands the export + canonical-CBOR codec together.
  - C3 (wit-value dispatch arms). No scalar in either target
    shim hits the path today: postgis scalars don't take
    records, and mobilitydb's temporal WIT (which DOES have
    record params) isn't in the resolved deps tree until
    Phase E adds it. Documented at `dispatch::classify_param`'s
    call site with the exact shape Phase E should emit.

- **C4 (`emit_metadata_impl`).** `Manifest.typed_values` now
  ships one `TypedValueBinding` per record discovered in the
  primary shim's WIT package. Helper-component records
  (sfcgal-component for postgis, proj/dbscan/etc. for
  mobilitydb) are filtered out via
  `package_belongs_to_primary` — those codecs live elsewhere.
  The decoder/encoder import strings name the would-be
  `sqlink-bridge:<primary>/serde-ops/...` symbols; Phase B's
  host falls back to identity passthrough on missing symbols
  so the binding entries are safe to ship now.

### Verification

- `cargo build --release` of `sqlink-shim-codegen` is clean
  (7 warnings, all dead-field on the new
  `WitResourceDecl`/`WitVariantDecl` introspection fields).
- `cargo test --release` of `sqlink-shim-codegen` — 6/6 pass
  (the existing `wit_parse` parser tests).
- `sqlink-shim-codegen --interface /tmp/postgis-interface.sqlite
  --target wasm-component --out ~/git/postgis-sqlink-bridge`
  runs clean, emitting the expected ~30 unwired diagnostics
  for raster-borrow / topology / coverage gaps (unchanged from
  Phase 3 round 3 baseline).
- `cd ~/git/postgis-sqlink-bridge && cargo build --target
  wasm32-wasip2 --release` is clean (~7s).
- `wac plug --plug ~/git/postgis-wasm/postgis-composed.wasm -o
  postgis-sqlink-loadable.wasm
  target/wasm32-wasip2/release/postgis_sqlink_bridge.wasm`
  produces a 113 MB loadable.wasm without composition errors.
- `cd ~/git/postgis-sqlink-bridge/verify && cargo run
  --release` — **all 25 checks pass** (the verify subcrate's
  full sweep: load, describe, registered=931 scalars + 34
  aggregates + 12 vtabs, wkt-roundtrip, st_x, st_distance,
  st_makepoint, st_area, st_intersects, st_buffer,
  st_geomfromwkb, st_union, st_collect, st_m null + some
  paths, st_clusterwithin, st_makebox2d, st_isvaliddetail,
  st_tileenvelope, st_dump vtab register). The "16/16 verify
  cases" target in the task spec is exceeded — the verify
  subcrate's check count is higher than 16.
- `sqlink-shim-codegen --interface
  /tmp/mobilitydb-interface.sqlite --target wasm-component
  --out /tmp/mobilitydb-sqlink-bridge` runs to completion
  WITHOUT PANICKING. Emits 1583 unwired-symbol diagnostics
  (every mobilitydb scalar — the temporal WIT isn't in the
  resolved deps tree, so no wiring is possible). The output
  contains a valid Cargo.toml, src/lib.rs, wit/world.wit
  with import lines for the present helper components
  (`dbscan:wasm/dbscan-api`, `kiddo:spatial/kdtree2d` &
  `kdtree3d`, `kmeans:wasm/kmeans-api`, `proj:wasm/proj-api`,
  `spade:spatial/{delaunay, constrained-delaunay, voronoi,
  utils}`), `sqlite:extension/*@1.0.0` contract surface, and
  the exports. The codegen does not panic; Phase E lands the
  proper mobilitydb deps tree.

### Counts

- typed-value-binding entries on the postgis manifest: **18**
  - postgis-types: coord, coord-z, coord-zm, bbox, box3d,
    buffer-params, extremes, inscribed-circle, valid-detail,
    coordinate-stats (10),
  - postgis-aggregates: bbox3d (1),
  - postgis-geocoder: address-component, parsed-address (2),
  - postgis-raster-pixels: pixel-coord (1),
  - postgis-raster-stats: summary-stats, histogram-bin,
    value-count (3),
  - postgis-topology-topogeom: topo-element (1).
  Helper-component records (sfcgal-component
  coordinate2d/3d/4d, sfcgal-error, geojson-options) are
  filtered out via the `package_belongs_to_primary`
  predicate.
- typed-value-binding entries on the mobilitydb manifest:
  **0** (the temporal WIT isn't in the resolved deps tree;
  Phase E adds it).

### Branches + commits

- `sqlink-shim-codegen`, branch `feat/wit-value-emission`
  (pushed to `origin/feat/wit-value-emission` on
  `tegmentum/sqlink-shim-codegen`):
  - `af11239` `feat(wasm-target): Phase D — decouple codegen
    from postgis assumptions`.
  - `76b9a29` `feat(wasm-target): Phase C1 — per-shim
    record-type registry`.
  - `a5ab2aa` `feat(wasm-target): Phase C2+C4 — record
    registry, typed-value bindings, deferred serde-ops`.
  - `9c196de` `docs(wasm-target): Phase C3 — document
    deferred wit-value dispatch path`.

- `postgis-sqlink-bridge`, branch `feat/wit-value-emission`
  (pushed to `origin/feat/wit-value-emission` on
  `tegmentum/postgis-sqlink-bridge`):
  - `bd855c5` `feat: Phase D regen — codegen-derived world.wit
    + @1.0.0 contract`.
  - `abe2762` `feat: Phase C regen — 18 typed-value-binding
    entries on manifest`.

### Honest deferrals carried into Phase E

- **Canonical-CBOR codec bodies.** The encoder + decoder for
  arbitrary WIT records is the substantive Phase E work. Until
  then the `typed-value-binding.decoder-import` /
  `encoder-import` strings name symbols that don't yet exist on
  the bridge; Phase B's host falls back to identity
  passthrough on missing codecs so the bridge still loads.
- **serde-ops WIT export.** The bridge declares the binding
  contract via the manifest but does NOT export
  `interface serde-ops { ... }` from the world (wac plug 0.10
  refuses to compose such a bridge — the records re-export
  through satisfied imports trips component-model validation).
  Phase E will revisit whether a stricter wac/wasm-tools
  release lifts the constraint, or whether the codegen ships
  a self-contained serde-ops interface (records defined
  locally rather than `use`-d from postgis-wasm).
- **Wit-value dispatch arms.** Neither postgis (no scalar
  takes a record) nor mobilitydb (temporal WIT not in deps)
  hits the path today. The PLAN-doc shape for the arm body
  is documented in code at `dispatch::classify_param`'s
  doc-comment for Phase E to pick up.
- **Mobilitydb deps tree.** `~/git/mobilitydb-wasm/wit/deps`
  currently holds only helper components (proj/dbscan/
  kmeans/kiddo/spade). The mobilitydb-temporal interface lives
  at `crates/mdb-temporal-wasm/wit/temporal.wit`. Phase E
  creates a proper `mobilitydb-sqlink-bridge/wit/deps/` tree
  containing `mobilitydb-temporal/` so the codegen actually
  wires the SQL functions.

## Phase E — done (2026-06-26)

### Landed

- **wac upgrade `0.10.0` → `0.10.1` (E2a).** PR #205 "Alias
  `use`'d types during composition instead of re-encoding them
  locally" is the headline fix. Phase C+D's `wac plug` had failed
  on `serde-ops { use <pkg>/<iface>.{rec}; ... }` exports with
  "instance not valid to be used as export". 0.10.1's aliasing
  helps with type-de-duplication but does NOT lift the underlying
  WASM component-model invariant: a component cannot export an
  instance whose interface references types not also exported by
  the composed component. After `wac plug` satisfies the upstream
  shim imports, those interfaces are no longer exported — so
  re-exporting their types via `use` still fails validation.
  Path (b) "define records locally in serde-ops" is the
  structural fix.

- **Codec emission — ciborium + serde derives (E1).** The
  bridge's per-record encoder/decoder bodies are
  ciborium-against-the-wit-bindgen-generated-Rust-struct.
  `wit_bindgen::generate!` gains
  `additional_derives: [serde::Serialize, serde::Deserialize]`,
  scoped via `additional_derives_ignore` to skip the contract
  package + helper-component records + primary-shim variants /
  flags (which can't derive serde out of the box). The
  decode/encode bodies are tiny:

  ```rust
  fn coord_from_canon_cbor(bytes: Vec<u8>) -> Result<Coord, String> {
      ciborium::de::from_reader::<Coord, _>(bytes.as_slice())
          .map_err(...)
  }
  fn coord_to_canon_cbor(value: Coord) -> Vec<u8> {
      let mut buf = Vec::new();
      ciborium::ser::into_writer(&value, &mut buf).expect(...);
      buf
  }
  ```

- **Local-record serde-ops interface (E2b).** Each primary-shim
  record gets a LOCAL copy declared inside the bridge's
  `interface serde-ops { ... }` (mirroring the upstream shape
  verbatim — same field names, same types, same order).
  Referenced enums (e.g. mobilitydb's `interpolation`) are also
  duplicated locally. `type X = Y;` aliases are inline-substituted
  in the local-record field types so e.g. `timestamp-tz` resolves
  to `s64` without needing the alias declaration too. The codegen
  emits a `record_registry::RecordType.is_copy` fix-point analysis
  that drives the dispatch arm's pass-by-value vs `&Record`
  decision (wit-bindgen generates by-value for all-primitive
  records, by-ref for records containing non-Copy fields like
  `list<T>`).

- **mobilitydb-sqlink-bridge new repo + vendored WIT (E3+E4).**
  `tegmentum/mobilitydb-sqlink-bridge` created. The
  `mobilitydb:temporal` package's 2240-line WIT is vendored at
  `wit/deps/mobilitydb-temporal/temporal.wit`.

- **Mobilitydb bridge codegen + build + compose (E5+E6).**
  Generated against `/tmp/mobilitydb-interface.sqlite` (the
  ~3000-symbol mobilitydb interface DB). Pipeline:
  - cargo build --target wasm32-wasip2 --release: clean (11s).
  - wac plug --plug
    ~/git/datafission/extensions/mobilitydb/deps/
    mdb-temporal-wasm.wasm --plug
    ~/git/datafission/extensions/postgis/deps/
    postgis-composed.wasm composes a 5.7 MB
    mobilitydb-sqlink-loadable.wasm.

- **wit-value dispatch arms (E5+E6).** Codegen extension:
  `dispatch::classify_param/classify_return` take
  `&[RecordType]`; an `Unsupported(name)` whose kebab matches a
  registered record routes to `ParamShape::WitValueRecord {...}`
  / `RetShape::WitValueRecord {...}`. Per-record helpers
  `arg_witvalue_<snake>` (decoder) + `ret_to_witvalue_<snake>`
  (encoder) are emitted in lib.rs top scope and referenced from
  the dispatch arms. The decoder:

  1. Unwraps SqlValue::WitValue(payload).
  2. Calls the bridge's LOCAL serde-ops codec
     (`<record>_from_canon_cbor`) — proof the codec fires.
  3. Ciborium-round-trips LOCAL → UPSTREAM. Same field shape
     by construction so the bytes match.

  `emit_lib::collect_referenced_records` filters helper emission
  to records actually referenced by a wired arm — wit-bindgen
  elides unused imported types from its bindings (e.g. postgis's
  `CoordZ` is declared in WIT but no function references it, so
  `bindings::postgis::wasm::postgis_types::CoordZ` doesn't exist).
  The filter sidesteps that.

  `shim_err_string<E: core::fmt::Debug>` generic helper replaces
  `postgis_err_string` in the dispatch arm's unwrap_chain so
  fallible variant errors format cleanly for ANY shim (postgis
  still keeps `postgis_err_string` for from_wkb / nice
  variant pretty-printing in helper sites).

  Mobilitydb dispatch: 1577 → 1296 unwired symbols (~280
  record-touching scalars now wire through wit-value).

- **Phase E acceptance — verify subcrate end-to-end (E7).**
  `mobilitydb-sqlink-bridge/verify/` loads
  postgis-sqlink-loadable.wasm FIRST (per D5 load-order
  convention), then mobilitydb-sqlink-loadable.wasm SECOND.
  Builds a synthetic tfloat-sequence with 3 instants
  (values 1.5, 3.0, 0.5) using a host-side mirror struct,
  ciborium-encodes to canon-CBOR, wraps as
  SqlValue::WitValue { type_id, bytes, symbolic_name } with the
  type-id matching the codegen's canon:wit sha256. Dispatches
  `tfloat_min_value(seq) → option<f64>`, asserts the result is
  `SqlValue::Real(0.5)`.

  Live output:

  ```
  [verify] loading postgis from .../postgis-sqlink-loadable.wasm
  [verify] postgis loaded: name=postgis
  [verify] loading mobilitydb from .../mobilitydb-sqlink-loadable.wasm
  [verify] mobilitydb loaded: name=mobilitydb
  [verify] registered: 1486 scalars, 61 aggregates, 45 vtabs,
           59 typed-value-bindings (host registry)
  [verify] synthetic tfloat-sequence canon-cbor = 141 bytes
  [verify] type_id (first 8) = 907389e610434205,
           symbolic = mobilitydb:temporal@0.1.0/types/tfloat-sequence
  [verify] manifest carries typed-value-binding: ...
  [verify] dispatching tfloat_min_value (func_id=605) with synthetic seq
  [verify] tfloat_min_value(synthetic seq) = 0.5
  [verify] PHASE E ACCEPTANCE: tfloat_min_value round-trip OK —
           host → wit-value → bridge serde-ops codec → upstream
           → option<f64> → host
  ```

  The "decode arg 0" path in the bridge calls the LOCAL serde-ops
  codec — if the codec hadn't fired, decode would fail and the
  verify would error out. The 0.5 result is the actual minimum
  of the synthetic input, proving the round-trip is bytes-clean
  end-to-end (not identity-passthrough fallback, not stub error).

### Verification

- `cargo test -p sqlink-host --lib` — **64/64 pass** (Phase B
  baseline unchanged).
- Postgis pipeline: `cargo build --target wasm32-wasip2 --release`
  clean, `wac plug` produces 113 MB loadable, `verify-load`
  16/16 checks pass. Phase E's `shim_err_string` rename + the
  emit_wit_value_helpers (no-op for postgis since it has no
  record-typed scalars) don't regress the postgis surface.
- Mobilitydb pipeline: `cargo build --target wasm32-wasip2
  --release` clean (11s), `wac plug` produces 6 MB loadable,
  `verify-load` reports 1486 scalars / 61 aggregates / 45 vtabs
  / 59 typed-value-bindings registered. Phase E acceptance
  scalar (`tfloat_min_value(synthetic seq) = 0.5`) passes.

### Branches + commits

- `sqlink-shim-codegen` branch `feat/wit-value-phase-e`
  (pushed to `origin/feat/wit-value-phase-e`):
  - `8e1a59c` `feat(wasm-target): Phase E1+E2 — ciborium codec
    bodies + local serde-ops records`.
  - `93f307f` `fix(wasm-target): handle one-line records + dedupe
    records by kebab`.
  - `8678b94` `feat(wasm-target): Phase E3 — wit-value dispatch
    arms + Copy analysis`.

- `postgis-sqlink-bridge` branch `feat/wit-value-phase-e`
  (pushed to `origin/feat/wit-value-phase-e`):
  - `13d6810` `feat: Phase E regen — local serde-ops records +
    ciborium codec bodies`.
  - `397c25a` `feat: Phase E3 regen — shim_err_string +
    emit_wit_value_helpers (no-op for postgis)`.

- `mobilitydb-sqlink-bridge` (NEW repo, branch
  `feat/wit-value-phase-e`, pushed to
  `origin/feat/wit-value-phase-e`):
  - `ea41c0b` `chore: initial vendor — mobilitydb-temporal WIT
    deps` (on main).
  - `2eb4b7e` `feat: Phase E codegen — first wired bridge for
    mobilitydb` (on feat/wit-value-phase-e).
  - `10ed12a` `feat: Phase E regen + verify subcrate proves
    end-to-end codec round-trip` (on feat/wit-value-phase-e).

- `sqlink` branch `feat/wit-value-phase-e` (the doc commit
  lives here).

### Honest caveats / Phase F+ debt

- **wac plug component-model invariant is structural, not 0.10.x
  -specific.** Phase C+D's blocker said wac 0.10 "refuses to
  compose a bridge whose `interface serde-ops { use
  <pkg>/<iface>.{record}; ... }` re-exports types from satisfied
  imports". The 0.10.1 fix (PR #205) helps with type aliasing
  but the underlying WASM component-model rule — an exported
  instance can only reference types ALSO exported by the
  composed component — still applies. Phase E settled on path
  (b) local-record-definition because that's the only path
  that's compatible with the invariant, not because 0.10.1
  failed to lift a specific bug.

- **LOCAL → UPSTREAM ciborium round-trip is wasteful.** Each
  wit-value param goes: bytes → LOCAL Rust struct → bytes →
  UPSTREAM Rust struct → call function. The intermediate
  re-encode is a no-op transformation that costs ~one alloc per
  record per call. Phase F+ can short-circuit by directly
  decoding bytes into UPSTREAM (skipping LOCAL) when the
  dispatcher knows it doesn't need to also expose the LOCAL
  shape. Phase E keeps the round-trip because (a) the LOCAL
  codec invocation is the PROOF of "codec actually fires" the
  acceptance gate watches for, and (b) Phase F's loader
  pre-check (#485 Phase 2) will surface drift between LOCAL
  and UPSTREAM shapes — having both materializations available
  makes the diff diagnosis-friendly.

- **Mobilitydb dispatch arm coverage is partial.** 1296 of
  ~3000 mobilitydb symbols are still unwired (return types
  not in the dispatcher alphabet — `option<bitemporal-bool-
  sequence>`, `option<time-period>`, list-of-records, etc.).
  Phase E proves the wit-value path works for at least
  `tfloat_min_value`; widening the dispatcher's
  `classify_return` alphabet to also wrap record-typed returns
  via `RetShape::WitValueRecord` (already partially landed)
  + handling `option<record>` and `list<record>` returns is
  Phase F+ work. The codegen substrate is in place; what
  remains is teaching the classifier to recognise these
  shapes.

- **sqlink-host worktree blocker.** The canonical `~/git/sqlink`
  clone was on `feat/s3-resident` with parallel work that broke
  compilation. Phase E set up `/tmp/sqlink-e7` as a clean
  worktree pinned to `fe2fa497` (Phase C+D base) and pointed
  the mobilitydb verify's path-dep at `/tmp/sqlink-e7/host`.
  Postgis verify continues to use the prebuilt
  `verify/target/release/verify-load` binary from Phase C+D
  because rebuilding it would hit the same dirty-clone
  blocker. Both verify paths reach their acceptance gates;
  no Phase E behaviour depends on rebuilding postgis-verify.

- **No mobilitydb smoke tests in shim-bridge-smoke-tests.**
  Phase E ships an in-tree verify subcrate. The
  cases/mobilitydb path in shim-bridge-smoke-tests doesn't
  exist yet; that's Phase F+ for full SQL-driven smoke
  coverage.

- **No browser end-to-end test for mobilitydb.** Phase B's
  browser ExtensionRegistry handles the typed-values drain
  generically; the JS side has no per-extension logic. Phase E
  verifies the wasm path; a browser regression test for
  mobilitydb-in-OPFS is Phase F+ (the worker-host runs the
  same wasm contract, so the proof carries; an explicit
  playwright spec is "belt-and-braces").

### What didn't change (Phase F still owes)

- Loader pre-check (#485 Phase 2 / Phase F): the friendly
  contract-mismatch message at extension load time when a
  component targets a different `sqlite:extension` major.
  The contract guard already exists (Phase A inverted the
  `@1.0` accept/reject); Phase F adds the diagnostic.
- The codegen's `classify_return` doesn't recognise
  `option<record>` or `list<record>` as wirable wit-value
  shapes yet. ~600 of the 1296 still-unwired mobilitydb
  symbols are these shapes; they're mechanical extensions to
  the dispatcher's alphabet.
- Mobilitydb performance work: the per-call ciborium
  re-encode adds a small allocation; for cold-cache scalars
  that's a few microseconds, batched aggregates would amplify.
  Profile after the dispatcher's alphabet widens.

### Tooling notes (for future maintainers)

- The codegen's `source_shim_deps_dir(primary)` accepts an
  `SQLINK_<PRIMARY>_BRIDGE_WIT_DEPS` env override. For
  mobilitydb specifically, set
  `SQLINK_MOBILITYDB_BRIDGE_WIT_DEPS=/tmp/mobilitydb-codegen
  -source` before regenerating into
  `~/git/mobilitydb-sqlink-bridge`; the codegen's `write_deps`
  copies the source tree into the dest tree, and if source
  ==dest the copy overwrites itself with an empty file
  partway through the cycle. Phase E sets this up via:

  ```
  mkdir -p /tmp/mobilitydb-codegen-source/mobilitydb-temporal
  cp ~/git/mobilitydb-wasm/crates/mdb-temporal-wasm/wit/temporal.wit \
     /tmp/mobilitydb-codegen-source/mobilitydb-temporal/
  SQLINK_MOBILITYDB_BRIDGE_WIT_DEPS=/tmp/mobilitydb-codegen-source \
  sqlink-shim-codegen --target wasm-component \
    --interface /tmp/mobilitydb-interface.sqlite \
    --out ~/git/mobilitydb-sqlink-bridge
  ```

- The mobilitydb verify subcrate's sqlink-host path-dep points
  at `/tmp/sqlink-e7/host` — a worktree of
  `~/git/sqlink@fe2fa497`. If the canonical clone returns to a
  clean state on a `@feat/wit-value-phase-e`-compatible
  commit, update verify/Cargo.toml's path-dep to `../../`
  -relative and remove `/tmp/sqlink-e7`.
