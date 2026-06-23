// Build the imports object that the jco-transpiled composed
// `cli + sqlite-lib` runtime expects, satisfying:
//
//   - wasi:cli/*, wasi:io/*, wasi:clocks/*, wasi:filesystem/*,
//     wasi:random/insecure-seed  — @tegmentum/wasi-polyfill plugins
//   - sqlink:wasm/extension-loader@0.1.0  — buildExtensionLoader()
//   - sqlite:extension/*  — SHOULD be satisfied internally by the
//     wac composition recipe. If any appear in the polyfill's
//     "missing imports" report at load time, the composition recipe
//     needs fixing (see browser/src/IMPORTS.md).
//
// The shape jco's async-mode `instantiate(getCoreModule, imports)`
// wants is an object keyed by un-versioned import names
// (`'wasi:cli/stdin'`, not `'wasi:cli/stdin@0.2.6'`) when
// `jcoCompat: true`. We pass that through forInterfaces().

import { createPolyfill, createPolicy } from '@tegmentum/wasi-polyfill/wasip2'
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
import {
  environmentPlugin,
  exitPlugin,
  stdinPlugin,
  stdoutPlugin,
  stderrPlugin,
  terminalInputPlugin,
  terminalOutputPlugin,
  terminalStdinPlugin,
  terminalStdoutPlugin,
  terminalStderrPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/cli'

import { buildExtensionLoader } from './extension-loader.js'

/**
 * Build the imports object jco's async-mode transpile wants for the
 * composed cli+sqlite-lib component.
 *
 * `stdinContent` pre-loads the cli's stdin with a script (typically
 * SQL + ".quit"); `onStdout` / `onStderr` capture the cli's writes
 * for the host to parse. Without these the polyfill's default stdin
 * is `EmptyInputStream` (immediate EOF) and the cli exits before
 * processing any SQL.
 *
 * @param {{
 *   registry: import('./extension-loader.js').ExtensionRegistry,
 *   stdinContent?: string | Uint8Array,
 *   onStdout?: (data: Uint8Array) => void,
 *   onStderr?: (data: Uint8Array) => void,
 *   env?: Record<string, string>,
 *   args?: string[],
 * }} opts
 * @returns {Promise<{ imports: Record<string, unknown>, polyfill: import('@tegmentum/wasi-polyfill/wasip2').Polyfill }>}
 */
export async function buildCliHostImports(opts) {
  // ConfigurablePolicy (via createPolicy) is what makes per-interface
  // options actually flow into the plugin Implementation factories.
  // AllowAllPolicy.configure() returns `{}` and silently drops every
  // option, which is why scripted stdin / stdout-capture never worked
  // until this fix.
  const overrides = []
  if (opts.stdinContent !== undefined) {
    overrides.push({
      interface: 'wasi:cli/stdin@0.2.6',
      options: { stdinContent: opts.stdinContent },
    })
  }
  if (opts.onStdout) {
    overrides.push({
      interface: 'wasi:cli/stdout@0.2.6',
      options: { onStdout: opts.onStdout },
    })
  }
  if (opts.onStderr) {
    overrides.push({
      interface: 'wasi:cli/stderr@0.2.6',
      options: { onStderr: opts.onStderr },
    })
  }
  const policy = createPolicy({ defaultAllow: true, overrides })
  const polyfill = createPolyfill({ policy })

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
  polyfill.registerPlugin(environmentPlugin)
  polyfill.registerPlugin(exitPlugin)
  polyfill.registerPlugin(stdinPlugin)
  polyfill.registerPlugin(stdoutPlugin)
  polyfill.registerPlugin(stderrPlugin)
  polyfill.registerPlugin(terminalInputPlugin)
  polyfill.registerPlugin(terminalOutputPlugin)
  polyfill.registerPlugin(terminalStdinPlugin)
  polyfill.registerPlugin(terminalStdoutPlugin)
  polyfill.registerPlugin(terminalStderrPlugin)

  const { imports } = await polyfill.forInterfaces(
    [
      'wasi:cli/environment@0.2.6',
      'wasi:cli/exit@0.2.6',
      'wasi:cli/stdin@0.2.6',
      'wasi:cli/stdout@0.2.6',
      'wasi:cli/stderr@0.2.6',
      'wasi:cli/terminal-input@0.2.6',
      'wasi:cli/terminal-output@0.2.6',
      'wasi:cli/terminal-stdin@0.2.6',
      'wasi:cli/terminal-stdout@0.2.6',
      'wasi:cli/terminal-stderr@0.2.6',
      'wasi:clocks/monotonic-clock@0.2.6',
      'wasi:clocks/wall-clock@0.2.6',
      'wasi:io/error@0.2.6',
      'wasi:io/poll@0.2.6',
      'wasi:io/streams@0.2.6',
      'wasi:filesystem/types@0.2.6',
      'wasi:filesystem/preopens@0.2.6',
      'wasi:random/insecure-seed@0.2.6',
    ],
    { jcoCompat: true, throwOnMissing: false, throwOnDenied: false },
  )

  // sqlink:wasm/extension-loader — host-implemented dynamic loader.
  imports['sqlink:wasm/extension-loader'] = buildExtensionLoader(opts.registry)

  // STAGE 8 NOTE: the composed component currently lists
  // sqlite:extension/* (http, policy, types, metadata, spi-loader)
  // as top-level imports — see browser/src/IMPORTS.md. These should
  // be wired internally by the wac composition recipe; if they're
  // not, the cli traps when it calls into spi-loader. Provide
  // structured-error stubs so the failure mode is "loader-error"
  // not "JS Error rethrown as trap".
  function stubInterface(name) {
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
              message: `${name}.${String(key)} not implemented in browser scenario 3 v1`,
            }
            throw err
          }
        },
      },
    )
  }
  for (const k of [
    'sqlite:extension/http',
    'sqlite:extension/policy',
    'sqlite:extension/types',
    'sqlite:extension/metadata',
    'sqlite:extension/spi-loader',
  ]) {
    if (!imports[k]) imports[k] = stubInterface(k)
  }

  return { imports, polyfill }
}
