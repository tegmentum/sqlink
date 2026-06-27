// PLAN-wit-value-extension.md Phase F (F4) — composed-cli-worker mirror
// of the host's sqlite:extension contract-version load guard.
//
// The native host (host/src/lib.rs) and sqlink-loader.dylib both pre-check
// every component's imported `sqlite:extension` package MAJOR against the
// host's `CONTRACT_MAJOR` BEFORE instantiation. The browser scenario runs
// the same composed-cli wasm, but through jco's runtime-bindgen rather
// than wasmtime — so it needs its own loader-time pre-check.
//
// Belt-and-suspenders rationale:
//   * Wasmtime would normally reject a contract-skewed component at
//     instantiate with a cryptic type-mismatch trap; jco doesn't have an
//     equivalent (it just transpiles and resolves names) and would happily
//     instantiate something whose marshalled values were corrupted by an
//     out-of-date discriminant layout.
//   * Bytes flowing through `loadExtension` came from the network /
//     OPFS / a developer hand-build — there's no upstream verifier here.
//     A loader-time check turns "silently misrun" into "loud actionable
//     error".
//
// Implementation: scan the component-model binary for ASCII strings of
// the shape `sqlite:extension/<iface>@<major>.<minor>.<patch>`. The
// component-model binary stores import names as length-prefixed UTF-8;
// the strings appear in plain bytes and a regex over the byte-decoded
// view finds them reliably. We extract the first match's major — every
// real component imports several `sqlite:extension/*` interfaces and they
// share a single package version.
//
// On mismatch (or no `sqlite:extension` import at all = legacy/unversioned),
// throw a friendly error matching the host's PLAN-wit-contract-versioning
// Phase 2 message shape so logs are consistent across all three loaders.

/**
 * The MAJOR of the `sqlite:extension` WIT contract this worker speaks.
 * Must stay in sync with `host/src/lib.rs::CONTRACT_MAJOR` (Phase A
 * landed the @1.0.0 bump from @0.1.0; the wit-value variant addition
 * was the breaking ABI change that warranted it).
 *
 * @type {number}
 */
export const CONTRACT_MAJOR = 1

/** The WIT contract package name this guard introspects. */
export const CONTRACT_PACKAGE = 'sqlite:extension'

/** Human-readable identifier the worker reports up to the main thread. */
export function contractVersionString() {
  return `${CONTRACT_PACKAGE}@${CONTRACT_MAJOR}.x`
}

// Pattern: `sqlite:extension/<iface>@<major>.<minor>.<patch>` where iface
// is a normal WIT interface name. Bytes are length-prefixed UTF-8 in the
// component-model binary so the strings appear verbatim; we Latin1-decode
// the bytes (every printable ASCII is identity in Latin1) before regexing
// to avoid TextDecoder's invalid-UTF8 throws on adjacent binary noise.
const CONTRACT_IMPORT_RE =
  /sqlite:extension\/[A-Za-z0-9_\-]+@(\d+)\.(\d+)\.(\d+)/

const LATIN1_DECODER = new TextDecoder('latin1')

/**
 * Walk a component-model wasm binary and return the imported
 * `sqlite:extension` MAJOR — or null when no such import is found (the
 * legacy/unversioned case, which is also rejected).
 *
 * @param {Uint8Array | ArrayBuffer} bytes
 * @returns {number | null}
 */
export function componentContractMajor(bytes) {
  const view =
    bytes instanceof Uint8Array
      ? bytes
      : new Uint8Array(bytes)
  // Latin1 round-trip keeps every byte addressable as a 1-char codepoint
  // — safe for substring/regex over the raw bytes without UTF-8 throws.
  const asString = LATIN1_DECODER.decode(view)
  const m = CONTRACT_IMPORT_RE.exec(asString)
  if (!m) return null
  const major = Number(m[1])
  return Number.isFinite(major) ? major : null
}

/**
 * Pre-check a component's contract major against the host's. Throws a
 * model-level Error with the PLAN-wit-contract-versioning Phase 2
 * message on mismatch (or on unversioned/legacy). Returns silently on
 * match.
 *
 * Matches the wording in `datalink-contract::check_component_contract`
 * so the native host, sqlink-loader, and browser worker all surface the
 * same error string — operators can grep once.
 *
 * @param {Uint8Array | ArrayBuffer} bytes  Raw component-model bytes.
 * @param {string} extName                  Extension/component name (for the message).
 * @param {number} [hostMajor=CONTRACT_MAJOR]
 */
export function checkComponentContract(bytes, extName, hostMajor = CONTRACT_MAJOR) {
  const importedMajor = componentContractMajor(bytes)
  if (importedMajor === hostMajor) return
  if (importedMajor === null) {
    throw new Error(
      `extension '${extName}' targets an UNVERSIONED ${CONTRACT_PACKAGE} ` +
        `contract (legacy, pre-versioning) but this host speaks contract ` +
        `${hostMajor}.x; rebuild it against the current WIT (or use the ` +
        `matching host version)`,
    )
  }
  throw new Error(
    `extension '${extName}' targets ${CONTRACT_PACKAGE} contract ` +
      `${importedMajor}.x but this host speaks contract ${hostMajor}.x; ` +
      `rebuild it against the current WIT (or use the matching host version)`,
  )
}
