// Build the "imports" object that jco's transpiled-with-
// `--instantiation async` extension wants, satisfying each
// `wasi:*` interface from @tegmentum/wasi-polyfill plugins.
//
// The sqlink extensions we ship are pure-compute scalars; they
// genuinely import wasi:cli/stdio + clocks + random + io because
// rust's wit-bindgen always emits the full WASI surface, but they
// don't actually exercise stdio (no .write()), they just need the
// import resolved so instantiation can complete. So we wire the
// real wasi-polyfill providers where they exist (random, clocks,
// io) and stub the cli surface to noops.

import {
  createPolyfill,
  AllowAllPolicy,
  createRuntimeBindgen,
} from '@tegmentum/wasi-polyfill/wasip2'
import {
  randomPlugin,
  insecureRandomPlugin,
  insecureSeedPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/random'
import {
  monotonicClockPlugin,
  wallClockPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/clocks'
import {
  errorPlugin,
  pollPlugin,
  streamsPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/io'
import {
  filesystemTypesPlugin,
  filesystemPreopensPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/filesystem'

let cachedPolyfill = null
let cachedImports = null

/**
 * Canonical list of `sqlite:extension/*` interfaces we stub for the
 * browser scenario. These are interfaces extensions IMPORT that are
 * either host-provided in native deployments (and we don't bother to
 * polyfill in the browser yet) or are pure SPI sinks the extension
 * doesn't actually call on the browser code path.
 *
 * `sqlite:extension/types` and `sqlite:extension/policy` are stubbed
 * only by the runtime-bindgen path (`buildExtensionAdditionalImports`)
 * — the `buildExtensionImports` path lets jco's own bindgen stub them
 * if the component declares them.
 *
 * Newer entries (s3-base, wal-frames, metadata, wal-hook, spi-loader)
 * resolve #444: the original list pre-dated those interfaces and
 * extensions like hookprobe that import them failed instantiation with
 * a TypeError on the missing import key. Stubbing them returns a
 * structured `SQLITE_ERROR` to any code path that actually calls a
 * stubbed function — the assertion being that pure-compute hookprobe
 * surfaces (scalar drain-log, wal-hook, update/commit/authorizer)
 * don't reach the stubbed call sites in the browser-test workload.
 */
const EXTENSION_IMPORT_STUB_NAMES = [
  'sqlite:extension/spi',
  'sqlite:extension/spi-loader',
  'sqlite:extension/session',
  'sqlite:extension/logging',
  'sqlite:extension/config',
  'sqlite:extension/http',
  'sqlite:extension/dns',
  'sqlite:extension/cache',
  'sqlite:extension/state',
  'sqlite:extension/random',
  'sqlite:extension/text',
  'sqlite:extension/hashing',
  'sqlite:extension/encoding',
  'sqlite:extension/prepared',
  'sqlite:extension/transaction',
  'sqlite:extension/schema',
  'sqlite:extension/metadata',
  'sqlite:extension/s3-base',
  'sqlite:extension/wal-frames',
  'sqlite:extension/wal-hook',
]

/**
 * Build the imports map jco's async-mode transpile expects.
 *
 * Cached across all extensions: every extension imports the same
 * WASI surface, so we instantiate once and reuse. (The polyfill
 * itself dedupes plugin instances internally, but caching the
 * imports object too avoids the per-call build cost.)
 */
export async function buildExtensionImports() {
  if (cachedImports) return cachedImports

  // AllowAllPolicy is fine here: the policy gate decides which
  // interface the polyfill EXPOSES; what the extension actually
  // gets is determined by what we register below. CLI imports we
  // don't register fall through to the stubs.
  const polyfill = createPolyfill({
    policy: new AllowAllPolicy(),
  })
  polyfill.registerPlugin(randomPlugin)
  polyfill.registerPlugin(insecureRandomPlugin)
  polyfill.registerPlugin(insecureSeedPlugin)
  polyfill.registerPlugin(monotonicClockPlugin)
  polyfill.registerPlugin(wallClockPlugin)
  polyfill.registerPlugin(errorPlugin)
  polyfill.registerPlugin(pollPlugin)
  polyfill.registerPlugin(streamsPlugin)
  polyfill.registerPlugin(filesystemTypesPlugin)
  polyfill.registerPlugin(filesystemPreopensPlugin)
  cachedPolyfill = polyfill

  // jcoCompat: true => un-versioned keys ('wasi:random/random'),
  // which is what jco's --instantiation async emits.
  const { imports } = await polyfill.forInterfaces(
    [
      'wasi:random/random@0.2.0',
      'wasi:random/insecure@0.2.0',
      'wasi:random/insecure-seed@0.2.0',
      'wasi:clocks/monotonic-clock@0.2.0',
      'wasi:clocks/wall-clock@0.2.0',
      'wasi:io/error@0.2.0',
      'wasi:io/poll@0.2.0',
      'wasi:io/streams@0.2.0',
      'wasi:filesystem/types@0.2.0',
      'wasi:filesystem/preopens@0.2.0',
    ],
    { jcoCompat: true, throwOnMissing: false, throwOnDenied: false },
  )

  // Stub the rest. Most sqlink extensions import the entire wasi:cli
  // surface even when they don't use it (rust wit-bindgen `generate_all`
  // emits the full world). Each function body throws if called — the
  // scalar code paths don't hit them, so we only pay if an extension
  // actually misbehaves, in which case loud is better than silent.
  function noop(name) {
    return new Proxy(
      {},
      {
        get(_t, key) {
          if (typeof key === 'symbol') return undefined
          // wit-bindgen + jco emit "TerminalInput" / "TerminalOutput"
          // / "Pollable" / etc. as class-bearing fields. Returning a
          // dummy class lets the bindgen-side `new` calls succeed if
          // they happen during eager construction; instance methods
          // throw on use.
          if (/^[A-Z]/.test(String(key))) {
            return class StubResource {}
          }
          // For host functions whose host-impl returns
          // `result<T, sqlite-error>`, jco wraps a thrown
          // payload-bearing object into the err arm. We emit a
          // shape-matched sqlite-error so the extension can
          // either fall through gracefully OR see a structured
          // failure — instead of getting a JS Error rethrown
          // which it'd interpret as a trap.
          return (..._args) => {
            const err = new Object()
            err.payload = {
              code: 1, // SQLITE_ERROR
              extendedCode: 1,
              message: `sqlink-browser scenario-3: ${name}.${String(key)} not implemented`,
            }
            throw err
          }
        },
      },
    )
  }
  const cliStubs = [
    'wasi:cli/environment',
    'wasi:cli/exit',
    'wasi:cli/stderr',
    'wasi:cli/stdin',
    'wasi:cli/stdout',
    'wasi:cli/terminal-input',
    'wasi:cli/terminal-output',
    'wasi:cli/terminal-stderr',
    'wasi:cli/terminal-stdin',
    'wasi:cli/terminal-stdout',
  ]
  for (const k of cliStubs) {
    if (!imports[k]) imports[k] = noop(k)
  }
  // Stub the sqlink-shaped imports that show up in extensions that
  // declare extra capabilities — they're not used by the pure-compute
  // surface but bindgen emits them.
  //
  // Keep this list in sync with `EXTENSION_IMPORT_STUB_NAMES` (the
  // shared canonical set) — the runtime-bindgen path in
  // `buildExtensionAdditionalImports` consumes the same list.
  for (const k of EXTENSION_IMPORT_STUB_NAMES) {
    if (!imports[k]) imports[k] = noop(k)
  }

  cachedImports = imports
  return imports
}

/** Tear down the cached polyfill (test cleanup). */
export function destroyExtensionImports() {
  cachedImports = null
  if (cachedPolyfill) {
    try {
      cachedPolyfill.destroy()
    } catch {
      // ignore
    }
    cachedPolyfill = null
  }
}

/**
 * Build the additionalImports map an extension component needs that
 * are NOT satisfied by the WASI polyfill itself. The same set of
 * stubs we install for the pre-transpiled `instantiate()` path —
 * sqlite-extension subworlds that wit-bindgen emits but extensions
 * don't actually use at runtime.
 *
 * Used by the runtime-bindgen path: `createRuntimeBindgen` lets the
 * polyfill resolve WASI imports on demand and merges this map for
 * the non-WASI ones.
 */
export function buildExtensionAdditionalImports() {
  function noop(name) {
    return new Proxy(
      {},
      {
        get(_t, key) {
          if (typeof key === 'symbol') return undefined
          if (/^[A-Z]/.test(String(key))) {
            return class StubResource {}
          }
          return (..._args) => {
            const err = new Object()
            err.payload = {
              code: 1,
              extendedCode: 1,
              message: `sqlink-browser scenario-3: ${name}.${String(key)} not implemented`,
            }
            throw err
          }
        },
      },
    )
  }
  // Same canonical list `buildExtensionImports` uses, plus the few
  // host-shaped names (types, policy) the runtime-bindgen path also
  // exposes. Versioned key shape (sqlite:extension/foo@0.1.0) matches
  // what jco's runtime transpile emits when the component declares
  // versioned imports — different from the unversioned shape the
  // build-time --instantiation async path uses. We register BOTH so
  // either consumer wires through.
  const stubNames = [
    'sqlite:extension/types',
    'sqlite:extension/policy',
    ...EXTENSION_IMPORT_STUB_NAMES,
  ]
  const out = {}
  for (const k of stubNames) {
    const stub = noop(k)
    out[k] = stub
    // Versioned key as well — runtime jco emits 'sqlite:extension/types@0.1.0'
    // for components that declared the versioned import. Both keys point at
    // the same proxy.
    out[`${k}@0.1.0`] = stub
  }
  return out
}

/**
 * Build a Polyfill instance suitable for instantiating sqlink
 * extension components at runtime via `createRuntimeBindgen`. Mirrors
 * `buildExtensionImports`'s plugin registrations but exposes the
 * polyfill rather than a pre-resolved imports map, so the bindgen
 * can ask the polyfill for the exact WASI interfaces it needs from
 * the parsed component bytes.
 *
 * AllowAllPolicy because extensions don't run an opt-in capability
 * check at this layer — what they get is decided by what we register
 * here. Anything wit-bindgen emits but we DON'T register falls
 * through to the additionalImports stubs.
 */
function createExtensionPolyfill() {
  const polyfill = createPolyfill({
    policy: new AllowAllPolicy(),
  })
  polyfill.registerPlugin(randomPlugin)
  polyfill.registerPlugin(insecureRandomPlugin)
  polyfill.registerPlugin(insecureSeedPlugin)
  polyfill.registerPlugin(monotonicClockPlugin)
  polyfill.registerPlugin(wallClockPlugin)
  polyfill.registerPlugin(errorPlugin)
  polyfill.registerPlugin(pollPlugin)
  polyfill.registerPlugin(streamsPlugin)
  polyfill.registerPlugin(filesystemTypesPlugin)
  polyfill.registerPlugin(filesystemPreopensPlugin)
  return polyfill
}

/**
 * Runtime transpile + instantiate a sqlink extension's
 * `.component.wasm` bytes. Builds a per-call Polyfill instance and
 * lets the polyfill resolve WASI imports from the parsed component;
 * non-WASI imports (sqlite:extension/*) are satisfied by
 * `buildExtensionAdditionalImports`.
 *
 * Returns `{ instance, bindgenResult }` — the same shape
 * `ExtensionRegistry.instantiateFromBytes` expects. The caller is
 * responsible for calling `bindgenResult.destroy()` on unload (the
 * registry's `delete()` does this).
 *
 * Why per-call (not cached): each extension's polyfill instance owns
 * resources (resource tables, plugin state). Sharing across all
 * extensions would conflate lifecycles — destroying one extension's
 * polyfill would invalidate others. Cheap enough: createPolyfill
 * itself is just object construction; the heavy work is jco's
 * transpile and that runs on bytes regardless.
 *
 * @param {Uint8Array | ArrayBuffer} bytes
 * @returns {Promise<{ instance: object, bindgenResult: object }>}
 */
export async function instantiateExtensionFromBytes(bytes) {
  const polyfill = createExtensionPolyfill()
  const bindgen = createRuntimeBindgen({
    polyfill,
    additionalImports: buildExtensionAdditionalImports(),
    jcoOptions: {
      name: 'extension',
      // Scalars are pure compute — no suspending imports today, so
      // sync mode is correct and avoids a JSPI dependency for the
      // (likely) common case. If a runtime extension ever needs
      // to block, this is where to flip to 'jspi' + async{Imports,
      // Exports}; pure-compute scalars stay sync.
      asyncMode: 'sync',
    },
  })
  const bindgenResult = await bindgen.instantiate(bytes)
  return { instance: bindgenResult.exports, bindgenResult }
}
