/**
 * Extension callbacks implementation for browser
 * Provides function implementations for SQLite extension system
 */

import type { SqlValue, UpdateType, AuthAction, AuthResult } from './sqlite/interfaces/sqlite-wasm-extension.js'

// Aggregate context storage
const aggregateContexts = new Map<bigint, unknown>()

// Hook registrations
const updateHooks = new Map<bigint, (op: UpdateType, database: string, table: string, rowid: bigint) => void>()
const commitHooks = new Map<bigint, () => boolean>()
const rollbackHooks = new Map<bigint, () => void>()

// Function implementations registry
const scalarFunctions = new Map<bigint, (args: SqlValue[]) => SqlValue>()
const aggregateFunctions = new Map<bigint, {
  step: (contextId: bigint, args: SqlValue[]) => void
  finalize: (contextId: bigint) => SqlValue
}>()
const collations = new Map<bigint, (a: string, b: string) => number>()

// Helper to create SqlValue
function nullValue(): SqlValue {
  return { valueType: 'null' }
}

function intValue(n: bigint): SqlValue {
  return { valueType: 'integer', intValue: n }
}

function floatValue(n: number): SqlValue {
  return { valueType: 'float', floatValue: n }
}

function textValue(s: string): SqlValue {
  return { valueType: 'text', textValue: s }
}

function _blobValue(b: Uint8Array): SqlValue {
  return { valueType: 'blob', blobValue: b }
}
// Re-export for potential future use
export { _blobValue as blobValue }

// Helper to extract values
function getText(v: SqlValue): string | null {
  if (v.valueType === 'null') return null
  if (v.valueType === 'text') return v.textValue ?? ''
  if (v.valueType === 'integer') return String(v.intValue)
  if (v.valueType === 'float') return String(v.floatValue)
  return null
}

function getInt(v: SqlValue): bigint | null {
  if (v.valueType === 'null') return null
  if (v.valueType === 'integer') return v.intValue ?? 0n
  if (v.valueType === 'float') return BigInt(Math.floor(v.floatValue ?? 0))
  return null
}

function getFloat(v: SqlValue): number | null {
  if (v.valueType === 'null') return null
  if (v.valueType === 'float') return v.floatValue ?? 0
  if (v.valueType === 'integer') return Number(v.intValue)
  return null
}

// ============================================================================
// Function IDs
// ============================================================================

// Original test function IDs (1-20)
export const FUNC_UUID = 1n
export const FUNC_REGEXP = 2n
export const FUNC_REVERSE = 3n
export const FUNC_MATH_SQRT = 4n
export const FUNC_GROUP_CONCAT = 10n
export const COLLATION_NOCASE_REVERSE = 20n

// Text extension function IDs (100+)
export const FUNC_TEXT_REVERSE = 141n
export const FUNC_TEXT_UPPER = 134n
export const FUNC_TEXT_LOWER = 135n
export const FUNC_REPEAT = 123n
export const FUNC_CHAR_LENGTH = 143n

// Extension Manager function IDs (200+)
export const FUNC_WASM_SYNC = 200n
export const FUNC_WASM_SEARCH = 201n
export const FUNC_WASM_LIST = 202n
export const FUNC_WASM_INSTALL = 203n
export const FUNC_WASM_UNINSTALL = 204n
export const FUNC_WASM_INFO = 205n
export const FUNC_WASM_UPDATE = 206n
export const FUNC_WASM_REGISTRY_VERSION = 207n
export const FUNC_WASM_INIT = 208n

// URL/HTTP function IDs (300+)
export const FUNC_FETCH_TEXT = 300n
export const FUNC_FETCH_JSON = 301n

// ============================================================================
// Extension Manager State
// ============================================================================

interface ExtensionRegistry {
  version: string
  extensions: Array<{
    name: string
    version: string
    description: string
    exports: string[]
    [key: string]: unknown
  }>
}

interface InstalledExtension {
  version: string
  installedAt: string
}

let extensionRegistry: ExtensionRegistry | null = null
let installedExtensions = new Map<string, InstalledExtension>()
let registryVersion = '1.0.0'
let lastSync: string | null = null

export function initializeExtensionManager(context: {
  registry?: ExtensionRegistry
  installed?: Record<string, InstalledExtension>
  lastSync?: string
}) {
  if (context.registry) {
    extensionRegistry = context.registry
    registryVersion = context.registry.version || '1.0.0'
  }
  if (context.installed) {
    installedExtensions = new Map(Object.entries(context.installed))
  }
  if (context.lastSync) {
    lastSync = context.lastSync
  }
}

export function markExtensionInstalled(name: string, version: string, installedAt?: string) {
  installedExtensions.set(name, {
    version,
    installedAt: installedAt || new Date().toISOString()
  })
}

export function markExtensionUninstalled(name: string) {
  installedExtensions.delete(name)
}

// ============================================================================
// Register Built-in Functions
// ============================================================================

// UUID v4 generation
scalarFunctions.set(FUNC_UUID, () => {
  const bytes = new Uint8Array(16)
  crypto.getRandomValues(bytes)
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const hex = Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join('')
  const uuid = `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
  return textValue(uuid)
})

// Regexp matching
scalarFunctions.set(FUNC_REGEXP, (args) => {
  const pattern = getText(args[0])
  const text = getText(args[1])
  if (pattern === null || text === null) return nullValue()
  try {
    const regex = new RegExp(pattern)
    return intValue(regex.test(text) ? 1n : 0n)
  } catch {
    return intValue(0n)
  }
})

// String reverse
scalarFunctions.set(FUNC_REVERSE, (args) => {
  const text = getText(args[0])
  if (text === null) return nullValue()
  return textValue([...text].reverse().join(''))
})

scalarFunctions.set(FUNC_TEXT_REVERSE, (args) => {
  const text = getText(args[0])
  if (text === null) return nullValue()
  return textValue([...text].reverse().join(''))
})

// Math sqrt
scalarFunctions.set(FUNC_MATH_SQRT, (args) => {
  const num = getFloat(args[0])
  if (num === null) return nullValue()
  return floatValue(Math.sqrt(num))
})

// Text upper/lower
scalarFunctions.set(FUNC_TEXT_UPPER, (args) => {
  const text = getText(args[0])
  if (text === null) return nullValue()
  return textValue(text.toUpperCase())
})

scalarFunctions.set(FUNC_TEXT_LOWER, (args) => {
  const text = getText(args[0])
  if (text === null) return nullValue()
  return textValue(text.toLowerCase())
})

// Repeat
scalarFunctions.set(FUNC_REPEAT, (args) => {
  const text = getText(args[0])
  const count = getInt(args[1])
  if (text === null || count === null) return nullValue()
  return textValue(text.repeat(Number(count)))
})

// Char length
scalarFunctions.set(FUNC_CHAR_LENGTH, (args) => {
  const text = getText(args[0])
  if (text === null) return nullValue()
  return intValue(BigInt([...text].length))
})

// ============================================================================
// Extension Manager Functions
// ============================================================================

scalarFunctions.set(FUNC_WASM_REGISTRY_VERSION, () => {
  return textValue(registryVersion)
})

scalarFunctions.set(FUNC_WASM_SYNC, () => {
  lastSync = new Date().toISOString()
  return textValue(JSON.stringify({
    success: true,
    extensions_count: extensionRegistry?.extensions.length ?? 0,
    last_sync: lastSync
  }))
})

scalarFunctions.set(FUNC_WASM_SEARCH, (args) => {
  const query = getText(args[0])?.toLowerCase() ?? ''
  if (!extensionRegistry) return textValue('[]')

  const results = extensionRegistry.extensions.filter(ext =>
    ext.name.toLowerCase().includes(query) ||
    ext.description.toLowerCase().includes(query) ||
    (ext as { keywords?: string[] }).keywords?.some((k: string) => k.toLowerCase().includes(query))
  ).map(ext => ({
    name: ext.name,
    version: ext.version,
    description: ext.description
  }))

  return textValue(JSON.stringify(results))
})

scalarFunctions.set(FUNC_WASM_LIST, () => {
  const list = Array.from(installedExtensions.entries()).map(([name, info]) => ({
    name,
    version: info.version,
    installed_at: info.installedAt
  }))
  return textValue(JSON.stringify(list))
})

scalarFunctions.set(FUNC_WASM_INSTALL, (args) => {
  const name = getText(args[0])
  if (!name) {
    return textValue(JSON.stringify({ success: false, error: 'Extension name required' }))
  }

  if (installedExtensions.has(name)) {
    return textValue(JSON.stringify({ success: false, error: `Extension '${name}' is already installed` }))
  }

  const ext = extensionRegistry?.extensions.find(e => e.name === name)
  if (!ext) {
    return textValue(JSON.stringify({ success: false, error: `Extension '${name}' not found in registry` }))
  }

  return textValue(JSON.stringify({
    success: true,
    action: 'install',
    name: ext.name,
    version: ext.version,
    message: `Extension '${name}' queued for installation`
  }))
})

scalarFunctions.set(FUNC_WASM_UNINSTALL, (args) => {
  const name = getText(args[0])
  if (!name) {
    return textValue(JSON.stringify({ success: false, error: 'Extension name required' }))
  }

  if (!installedExtensions.has(name)) {
    return textValue(JSON.stringify({ success: false, error: `Extension '${name}' is not installed` }))
  }

  return textValue(JSON.stringify({
    success: true,
    action: 'uninstall',
    name,
    message: `Extension '${name}' queued for uninstallation`
  }))
})

scalarFunctions.set(FUNC_WASM_INFO, (args) => {
  const name = getText(args[0])
  if (!name) {
    return textValue(JSON.stringify({ error: 'Extension name required' }))
  }

  const ext = extensionRegistry?.extensions.find(e => e.name === name)
  if (!ext) {
    return textValue(JSON.stringify({ error: `Extension '${name}' not found` }))
  }

  const installed = installedExtensions.get(name)
  return textValue(JSON.stringify({
    name: ext.name,
    version: ext.version,
    description: ext.description,
    exports: ext.exports,
    installed: !!installed,
    installed_version: installed?.version
  }))
})

scalarFunctions.set(FUNC_WASM_UPDATE, (args) => {
  const name = args.length > 0 ? getText(args[0]) : null

  if (name) {
    const installed = installedExtensions.get(name)
    if (!installed) {
      return textValue(JSON.stringify({ success: false, error: `Extension '${name}' is not installed` }))
    }

    const ext = extensionRegistry?.extensions.find(e => e.name === name)
    if (!ext) {
      return textValue(JSON.stringify({ success: false, error: `Extension '${name}' not found in registry` }))
    }

    if (installed.version === ext.version) {
      return textValue(JSON.stringify({ success: true, message: `Extension '${name}' is already at latest version` }))
    }

    return textValue(JSON.stringify({
      success: true,
      updates: [{
        name,
        old_version: installed.version,
        new_version: ext.version
      }]
    }))
  }

  // Check all installed extensions
  const updates: Array<{ name: string; old_version: string; new_version: string }> = []
  for (const [extName, info] of installedExtensions) {
    const ext = extensionRegistry?.extensions.find(e => e.name === extName)
    if (ext && ext.version !== info.version) {
      updates.push({
        name: extName,
        old_version: info.version,
        new_version: ext.version
      })
    }
  }

  return textValue(JSON.stringify({ success: true, updates }))
})

scalarFunctions.set(FUNC_WASM_INIT, () => {
  const sql = `
CREATE TABLE IF NOT EXISTS _wasm_extensions (
  name TEXT PRIMARY KEY,
  version TEXT NOT NULL,
  description TEXT,
  exports TEXT
);
CREATE TABLE IF NOT EXISTS _wasm_installed (
  name TEXT PRIMARY KEY,
  version TEXT NOT NULL,
  installed_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS _wasm_registry_meta (
  key TEXT PRIMARY KEY,
  value TEXT
);`
  return textValue(sql)
})

// Group concat aggregate
aggregateFunctions.set(FUNC_GROUP_CONCAT, {
  step: (contextId, args) => {
    const value = getText(args[0])
    const sep = args.length > 1 ? getText(args[1]) ?? ',' : ','
    if (value === null) return

    let ctx = aggregateContexts.get(contextId) as { values: string[]; sep: string } | undefined
    if (!ctx) {
      ctx = { values: [], sep }
      aggregateContexts.set(contextId, ctx)
    }
    ctx.values.push(value)
  },
  finalize: (contextId) => {
    const ctx = aggregateContexts.get(contextId) as { values: string[]; sep: string } | undefined
    aggregateContexts.delete(contextId)
    if (!ctx || ctx.values.length === 0) return nullValue()
    return textValue(ctx.values.join(ctx.sep))
  }
})

// Collation: case-insensitive reverse
collations.set(COLLATION_NOCASE_REVERSE, (a, b) => {
  return b.toLowerCase().localeCompare(a.toLowerCase())
})

// ============================================================================
// Callback Implementations (called by WASM)
// ============================================================================

export function onScalarFunction(functionId: bigint, args: SqlValue[]): SqlValue {
  const fn = scalarFunctions.get(functionId)
  if (!fn) {
    console.warn(`Unknown scalar function ID: ${functionId}`)
    return nullValue()
  }
  try {
    return fn(args)
  } catch (e) {
    console.error(`Error in scalar function ${functionId}:`, e)
    return nullValue()
  }
}

export function onAggregateStep(functionId: bigint, contextId: bigint, args: SqlValue[]): void {
  const fn = aggregateFunctions.get(functionId)
  if (!fn) {
    console.warn(`Unknown aggregate function ID: ${functionId}`)
    return
  }
  try {
    fn.step(contextId, args)
  } catch (e) {
    console.error(`Error in aggregate step ${functionId}:`, e)
  }
}

export function onAggregateFinalize(functionId: bigint, contextId: bigint): SqlValue {
  const fn = aggregateFunctions.get(functionId)
  if (!fn) {
    console.warn(`Unknown aggregate function ID: ${functionId}`)
    return nullValue()
  }
  try {
    return fn.finalize(contextId)
  } catch (e) {
    console.error(`Error in aggregate finalize ${functionId}:`, e)
    return nullValue()
  }
}

export function onCollationCompare(collationId: bigint, a: string, b: string): number {
  const fn = collations.get(collationId)
  if (!fn) {
    console.warn(`Unknown collation ID: ${collationId}`)
    return a.localeCompare(b)
  }
  return fn(a, b)
}

export function onUpdate(hookId: bigint, op: UpdateType, database: string, table: string, rowid: bigint): void {
  const hook = updateHooks.get(hookId)
  if (hook) {
    hook(op, database, table, rowid)
  }
}

export function onCommit(hookId: bigint): boolean {
  const hook = commitHooks.get(hookId)
  return hook ? hook() : false
}

export function onRollback(hookId: bigint): void {
  const hook = rollbackHooks.get(hookId)
  if (hook) hook()
}

export function onAuthorize(
  _authId: bigint,
  _action: AuthAction,
  _arg1: string | undefined,
  _arg2: string | undefined,
  _database: string | undefined,
  _trigger: string | undefined
): AuthResult {
  // Allow all by default
  return 'ok'
}

// Hook registration helpers
export function registerUpdateHook(hookId: bigint, callback: (op: UpdateType, database: string, table: string, rowid: bigint) => void) {
  updateHooks.set(hookId, callback)
}

export function registerCommitHook(hookId: bigint, callback: () => boolean) {
  commitHooks.set(hookId, callback)
}

export function registerRollbackHook(hookId: bigint, callback: () => void) {
  rollbackHooks.set(hookId, callback)
}

// ============================================================================
// WASM Extension Loading Support
// ============================================================================

// Loaded extension info
export interface LoadedWasmExtension {
  name: string
  version: string
  path: string
  functions: string[]
  module?: WebAssembly.Module
  instance?: WebAssembly.Instance
}

// Map of loaded WASM extensions
const loadedWasmExtensions = new Map<string, LoadedWasmExtension>()

/**
 * Load a WASM extension from a URL or path
 * This implements the extension-loader interface for browser environments
 */
export async function loadWasmExtension(path: string): Promise<LoadedWasmExtension> {
  // Extract extension name from path
  const basename = path.split('/').pop()?.split('\\').pop() || path
  const name = basename.replace(/\.wasm$/, '')

  if (loadedWasmExtensions.has(name)) {
    throw new Error(`Extension '${name}' is already loaded`)
  }

  // Fetch and compile the WASM module
  const response = await fetch(path)
  if (!response.ok) {
    throw new Error(`Failed to fetch extension: ${response.statusText}`)
  }

  const module = await WebAssembly.compileStreaming(response)

  // For now, we use a simple instantiation without linking
  // In a full implementation, we would link the extension to SQLite APIs
  const instance = await WebAssembly.instantiate(module, {})

  // Try to get extension info from exported functions
  let version = '1.0.0'
  const functions: string[] = []

  // Check for get-info export
  if (typeof (instance.exports as Record<string, unknown>)['get-info'] === 'function') {
    try {
      const info = ((instance.exports as Record<string, unknown>)['get-info'] as () => unknown)()
      if (info && typeof info === 'object') {
        version = (info as { version?: string }).version || version
      }
    } catch {
      // Ignore errors reading info
    }
  }

  // Collect exported function names
  for (const [key, value] of Object.entries(instance.exports)) {
    if (typeof value === 'function' && !key.startsWith('_')) {
      functions.push(key)
    }
  }

  const extension: LoadedWasmExtension = {
    name,
    version,
    path,
    functions,
    module,
    instance
  }

  loadedWasmExtensions.set(name, extension)
  return extension
}

/**
 * Unload a WASM extension by name
 */
export function unloadWasmExtension(name: string): boolean {
  if (!loadedWasmExtensions.has(name)) {
    return false
  }
  loadedWasmExtensions.delete(name)
  return true
}

/**
 * List all loaded WASM extensions
 */
export function listWasmExtensions(): LoadedWasmExtension[] {
  return Array.from(loadedWasmExtensions.values())
}

/**
 * Check if an extension is loaded
 */
export function isWasmExtensionLoaded(name: string): boolean {
  return loadedWasmExtensions.has(name)
}
