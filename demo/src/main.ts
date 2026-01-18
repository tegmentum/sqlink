import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'

import {
  registerCorePlugins,
  AllowAllPolicy,
  Polyfill,
  globalRegistry,
} from '@tegmentum/wasi-polyfill'
import {
  setGlobalStdioProvider,
  createXtermStdio,
} from '@tegmentum/wasi-polyfill/plugins/cli'
import { socketPlugins } from '@tegmentum/wasi-polyfill/plugins/sockets'

import { instantiate, type ImportObject, type Root } from './sqlite/sqlite.js'
import * as extensionCallbacks from './extension-callbacks.js'
import registry from './registry.json'

// Import core WASM modules as URLs
import sqliteCore from './sqlite/sqlite.core.wasm?url'
import sqliteCore2 from './sqlite/sqlite.core2.wasm?url'
import sqliteCore3 from './sqlite/sqlite.core3.wasm?url'
import sqliteCore4 from './sqlite/sqlite.core4.wasm?url'
import sqliteCore5 from './sqlite/sqlite.core5.wasm?url'

const loadingEl = document.getElementById('loading')!
const loadingText = document.getElementById('loading-text')!
const errorEl = document.getElementById('error')!
const containerEl = document.getElementById('terminal-container')!
const fileInput = document.getElementById('file-input') as HTMLInputElement
const btnUpload = document.getElementById('btn-upload')!
const btnDemo = document.getElementById('btn-demo')!
const btnExport = document.getElementById('btn-export')!
const btnSave = document.getElementById('btn-save')!
const btnLoad = document.getElementById('btn-load')!
const storageStatus = document.getElementById('storage-status')!

// OPFS storage constants
const OPFS_FILENAME = 'sqlite-wasm-demo.db'

function showError(message: string) {
  loadingEl.style.display = 'none'
  errorEl.style.display = 'block'
  errorEl.textContent = message
}

function updateLoadingText(text: string) {
  loadingText.textContent = text
}

// Map of core module names to their URLs
const coreModuleUrls: Record<string, string> = {
  'sqlite.core.wasm': sqliteCore,
  'sqlite.core2.wasm': sqliteCore2,
  'sqlite.core3.wasm': sqliteCore3,
  'sqlite.core4.wasm': sqliteCore4,
  'sqlite.core5.wasm': sqliteCore5,
}

// Cache for compiled modules
const compiledModules = new Map<string, WebAssembly.Module>()

async function getCoreModule(path: string): Promise<WebAssembly.Module> {
  const normalizedPath = path.replace(/^\.\//, '')

  if (compiledModules.has(normalizedPath)) {
    return compiledModules.get(normalizedPath)!
  }

  const url = coreModuleUrls[normalizedPath]
  if (!url) {
    throw new Error(`Unknown core module: ${path}`)
  }

  const response = await fetch(url)
  const module = await WebAssembly.compileStreaming(response)
  compiledModules.set(normalizedPath, module)

  return module
}

// OPFS helpers
async function hasOPFS(): Promise<boolean> {
  try {
    return 'getDirectory' in navigator.storage
  } catch {
    return false
  }
}

async function saveToOPFS(data: Uint8Array): Promise<void> {
  const root = await navigator.storage.getDirectory()
  const fileHandle = await root.getFileHandle(OPFS_FILENAME, { create: true })
  const writable = await fileHandle.createWritable()
  // Copy to a fresh ArrayBuffer to avoid TypeScript ArrayBufferLike issues
  const buffer = new ArrayBuffer(data.byteLength)
  new Uint8Array(buffer).set(data)
  await writable.write(buffer)
  await writable.close()
}

async function loadFromOPFS(): Promise<Uint8Array | null> {
  try {
    const root = await navigator.storage.getDirectory()
    const fileHandle = await root.getFileHandle(OPFS_FILENAME)
    const file = await fileHandle.getFile()
    const buffer = await file.arrayBuffer()
    return new Uint8Array(buffer)
  } catch {
    return null
  }
}

async function _deleteFromOPFS(): Promise<void> {
  try {
    const root = await navigator.storage.getDirectory()
    await root.removeEntry(OPFS_FILENAME)
  } catch {
    // File may not exist
  }
}
// Export for potential future use
export { _deleteFromOPFS as deleteFromOPFS }

// REPL class for interactive SQL
class SqliteRepl {
  private terminal: Terminal
  private sqlite: Root
  private db: bigint
  private inputBuffer = ''
  private history: string[] = []
  private historyIndex = -1
  private cursorPos = 0
  private prompt = 'sqlite> '
  private continuationPrompt = '   ...> '
  private inMultiline = false

  constructor(terminal: Terminal, sqlite: Root) {
    this.terminal = terminal
    this.sqlite = sqlite
    this.db = 0n
  }

  async init() {
    // Open in-memory database
    this.db = this.sqlite.lowLevel.open(':memory:', {
      readwrite: true,
      create: true,
      memory: true
    })

    // Register extension manager functions
    this.registerExtensions()

    // Initialize extension manager with registry
    extensionCallbacks.initializeExtensionManager({
      registry: registry as unknown as Parameters<typeof extensionCallbacks.initializeExtensionManager>[0]['registry'],
      installed: {},
      lastSync: new Date().toISOString()
    })

    this.showWelcome()
    this.showPrompt()
    this.setupInput()
  }

  private registerExtensions() {
    const ext = this.sqlite.extension

    // Extension manager functions
    ext.registerScalarFunction(this.db, 'wasm_registry_version', 0, { deterministic: true }, extensionCallbacks.FUNC_WASM_REGISTRY_VERSION)
    ext.registerScalarFunction(this.db, 'wasm_sync', 0, { deterministic: false }, extensionCallbacks.FUNC_WASM_SYNC)
    ext.registerScalarFunction(this.db, 'wasm_search', 1, { deterministic: false }, extensionCallbacks.FUNC_WASM_SEARCH)
    ext.registerScalarFunction(this.db, 'wasm_list', 0, { deterministic: false }, extensionCallbacks.FUNC_WASM_LIST)
    ext.registerScalarFunction(this.db, 'wasm_install', 1, { deterministic: false }, extensionCallbacks.FUNC_WASM_INSTALL)
    ext.registerScalarFunction(this.db, 'wasm_uninstall', 1, { deterministic: false }, extensionCallbacks.FUNC_WASM_UNINSTALL)
    ext.registerScalarFunction(this.db, 'wasm_info', 1, { deterministic: false }, extensionCallbacks.FUNC_WASM_INFO)
    ext.registerScalarFunction(this.db, 'wasm_update', 0, { deterministic: false }, extensionCallbacks.FUNC_WASM_UPDATE)
    ext.registerScalarFunction(this.db, 'wasm_init', 0, { deterministic: true }, extensionCallbacks.FUNC_WASM_INIT)

    // Utility functions
    ext.registerScalarFunction(this.db, 'uuid', 0, { deterministic: false }, extensionCallbacks.FUNC_UUID)
    ext.registerScalarFunction(this.db, 'regexp', 2, { deterministic: true }, extensionCallbacks.FUNC_REGEXP)
    ext.registerScalarFunction(this.db, 'reverse', 1, { deterministic: true }, extensionCallbacks.FUNC_REVERSE)
    ext.registerScalarFunction(this.db, 'math_sqrt', 1, { deterministic: true }, extensionCallbacks.FUNC_MATH_SQRT)

    // Text functions
    ext.registerScalarFunction(this.db, 'text_reverse', 1, { deterministic: true }, extensionCallbacks.FUNC_TEXT_REVERSE)
    ext.registerScalarFunction(this.db, 'text_upper', 1, { deterministic: true }, extensionCallbacks.FUNC_TEXT_UPPER)
    ext.registerScalarFunction(this.db, 'text_lower', 1, { deterministic: true }, extensionCallbacks.FUNC_TEXT_LOWER)
    ext.registerScalarFunction(this.db, 'repeat', 2, { deterministic: true }, extensionCallbacks.FUNC_REPEAT)
    ext.registerScalarFunction(this.db, 'char_length', 1, { deterministic: true }, extensionCallbacks.FUNC_CHAR_LENGTH)

    // Aggregate functions
    ext.registerAggregateFunction(this.db, 'group_concat_custom', 2, { deterministic: true }, extensionCallbacks.FUNC_GROUP_CONCAT)
  }

  private showWelcome() {
    this.terminal.writeln('\x1b[1;35mSQLite WASM Terminal\x1b[0m')
    this.terminal.writeln('\x1b[90mSQLite running in WebAssembly with extension support\x1b[0m')
    this.terminal.writeln('')
    this.terminal.writeln('\x1b[36mData loading:\x1b[0m')
    this.terminal.writeln('  .demo                   -- Load demo database')
    this.terminal.writeln('  .fetch <url>            -- Load SQL/CSV from URL')
    this.terminal.writeln('  Drag & drop SQL/CSV files onto terminal')
    this.terminal.writeln('')
    this.terminal.writeln('\x1b[36mBuilt-in functions:\x1b[0m')
    this.terminal.writeln('  uuid(), reverse(), math_sqrt(), regexp()')
    this.terminal.writeln('  text_upper(), text_lower(), repeat(), char_length()')
    this.terminal.writeln('')
    this.terminal.writeln('Type \x1b[33m.help\x1b[0m for commands, \x1b[33m.demo\x1b[0m to load sample data')
    this.terminal.writeln('')
  }

  private showPrompt() {
    this.terminal.write(this.inMultiline ? this.continuationPrompt : this.prompt)
  }

  private setupInput() {
    this.terminal.onData(data => {
      for (const char of data) {
        this.handleChar(char)
      }
    })
  }

  private handleChar(char: string) {
    const code = char.charCodeAt(0)

    if (char === '\r' || char === '\n') {
      this.terminal.writeln('')
      this.handleEnter()
    } else if (code === 127 || code === 8) {
      // Backspace
      if (this.cursorPos > 0) {
        this.inputBuffer = this.inputBuffer.slice(0, this.cursorPos - 1) + this.inputBuffer.slice(this.cursorPos)
        this.cursorPos--
        this.terminal.write('\b \b')
      }
    } else if (char === '\x1b[A') {
      // Up arrow - history
      if (this.historyIndex < this.history.length - 1) {
        this.historyIndex++
        this.replaceInput(this.history[this.history.length - 1 - this.historyIndex])
      }
    } else if (char === '\x1b[B') {
      // Down arrow - history
      if (this.historyIndex > 0) {
        this.historyIndex--
        this.replaceInput(this.history[this.history.length - 1 - this.historyIndex])
      } else if (this.historyIndex === 0) {
        this.historyIndex = -1
        this.replaceInput('')
      }
    } else if (char === '\x03') {
      // Ctrl+C
      this.terminal.writeln('^C')
      this.inputBuffer = ''
      this.cursorPos = 0
      this.inMultiline = false
      this.showPrompt()
    } else if (code >= 32) {
      // Printable characters
      this.inputBuffer = this.inputBuffer.slice(0, this.cursorPos) + char + this.inputBuffer.slice(this.cursorPos)
      this.cursorPos++
      this.terminal.write(char)
    }
  }

  private replaceInput(text: string) {
    // Clear current input
    while (this.cursorPos > 0) {
      this.terminal.write('\b \b')
      this.cursorPos--
    }
    // Write new input
    this.inputBuffer = text
    this.cursorPos = text.length
    this.terminal.write(text)
  }

  private async handleEnter() {
    const input = this.inputBuffer.trim()
    this.inputBuffer = ''
    this.cursorPos = 0

    if (!input) {
      this.inMultiline = false
      this.showPrompt()
      return
    }

    // Check for meta commands
    if (input.startsWith('.')) {
      await this.handleMetaCommand(input)
      this.showPrompt()
      return
    }

    // Check if statement is complete
    if (!input.endsWith(';') && !this.inMultiline) {
      this.inMultiline = true
      this.inputBuffer = input + ' '
      this.cursorPos = this.inputBuffer.length
      this.showPrompt()
      this.terminal.write(this.inputBuffer)
      return
    }

    if (this.inMultiline) {
      this.inMultiline = false
    }

    // Add to history
    if (input) {
      this.history.push(input)
      this.historyIndex = -1
    }

    // Execute SQL
    this.executeSql(input)
    this.showPrompt()
  }

  private async handleMetaCommand(cmd: string) {
    const parts = cmd.match(/(?:[^\s"]+|"[^"]*")+/g)
    if (!parts || parts.length === 0) return
    const command = parts[0].toLowerCase()

    switch (command) {
      case '.help':
        this.terminal.writeln('\x1b[36mMeta Commands:\x1b[0m')
        this.terminal.writeln('  .help              Show this help')
        this.terminal.writeln('  .tables            List tables')
        this.terminal.writeln('  .schema [table]    Show schema')
        this.terminal.writeln('  .fetch <url>       Load SQL/CSV from URL')
        this.terminal.writeln('  .import <csv> <table>  Import CSV file to table')
        this.terminal.writeln('  .demo              Load demo database')
        this.terminal.writeln('  .export            Export database as SQL')
        this.terminal.writeln('  .save              Save to browser storage')
        this.terminal.writeln('  .load              Load from browser storage')
        this.terminal.writeln('  .clear             Clear screen')
        this.terminal.writeln('  .quit              Exit')
        this.terminal.writeln('')
        this.terminal.writeln('\x1b[36mWASM Extension Commands:\x1b[0m')
        this.terminal.writeln('  .loadext <url>     Load WASM extension from URL')
        this.terminal.writeln('  .unloadext <name>  Unload WASM extension')
        this.terminal.writeln('  .extensions        List loaded WASM extensions')
        break

      case '.tables':
        this.executeSql("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name;")
        break

      case '.schema':
        if (parts[1]) {
          this.executeSql(`SELECT sql FROM sqlite_master WHERE name='${parts[1]}';`)
        } else {
          this.executeSql("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name;")
        }
        break

      case '.fetch':
        if (!parts[1]) {
          this.terminal.writeln('\x1b[31mUsage: .fetch <url>\x1b[0m')
          this.terminal.writeln('  Example: .fetch https://example.com/data.sql')
        } else {
          await this.loadFromUrl(parts[1].replace(/^"|"$/g, ''))
        }
        break

      case '.demo':
        await this.loadDemo()
        break

      case '.export':
        const sql = this.exportSql()
        if (sql) {
          const blob = new Blob([sql], { type: 'application/sql' })
          const url = URL.createObjectURL(blob)
          const a = document.createElement('a')
          a.href = url
          a.download = 'export.sql'
          a.click()
          URL.revokeObjectURL(url)
          this.terminal.writeln('\x1b[32mDatabase exported to export.sql\x1b[0m')
        }
        break

      case '.save':
        await this.saveToStorage()
        break

      case '.load':
        await this.loadFromStorage()
        break

      case '.quit':
      case '.exit':
        this.terminal.writeln('\x1b[90mGoodbye!\x1b[0m')
        this.sqlite.lowLevel.close(this.db)
        break

      case '.clear':
        this.terminal.clear()
        break

      case '.loadext':
        if (!parts[1]) {
          this.terminal.writeln('\x1b[31mUsage: .loadext <url>\x1b[0m')
          this.terminal.writeln('  Example: .loadext /extensions/fts5.wasm')
        } else {
          await this.loadWasmExtension(parts[1].replace(/^"|"$/g, ''))
        }
        break

      case '.unloadext':
        if (!parts[1]) {
          this.terminal.writeln('\x1b[31mUsage: .unloadext <name>\x1b[0m')
        } else {
          const unloaded = extensionCallbacks.unloadWasmExtension(parts[1])
          if (unloaded) {
            this.terminal.writeln(`\x1b[32mUnloaded extension: ${parts[1]}\x1b[0m`)
          } else {
            this.terminal.writeln(`\x1b[31mExtension '${parts[1]}' not found\x1b[0m`)
          }
        }
        break

      case '.extensions':
        const exts = extensionCallbacks.listWasmExtensions()
        if (exts.length === 0) {
          this.terminal.writeln('\x1b[90mNo WASM extensions loaded\x1b[0m')
          this.terminal.writeln('Use .loadext <url> to load a WASM extension')
        } else {
          this.terminal.writeln('\x1b[36mLoaded WASM Extensions:\x1b[0m')
          for (const ext of exts) {
            this.terminal.writeln(`  ${ext.name} v${ext.version}`)
            this.terminal.writeln(`    Path: ${ext.path}`)
            this.terminal.writeln(`    Functions: ${ext.functions.join(', ') || '(none exported)'}`)
          }
        }
        break

      default:
        this.terminal.writeln(`\x1b[31mUnknown command: ${command}\x1b[0m`)
    }
  }

  private executeSql(sql: string) {
    try {
      const stmt = this.sqlite.lowLevel.prepare(this.db, sql)

      // Get column names
      const colCount = this.sqlite.lowLevel.columnCount(stmt)
      const columns: string[] = []
      for (let i = 0; i < colCount; i++) {
        columns.push(this.sqlite.lowLevel.columnName(stmt, i))
      }

      // Collect rows
      const rows: string[][] = []
      let stepResult = this.sqlite.lowLevel.step(stmt)

      while (stepResult === 'row') {
        const row: string[] = []
        for (let i = 0; i < colCount; i++) {
          const type = this.sqlite.lowLevel.getColumnType(stmt, i)
          let value: string

          switch (type) {
            case 'null':
              value = 'NULL'
              break
            case 'integer':
              value = String(this.sqlite.lowLevel.columnInt64(stmt, i))
              break
            case 'float':
              value = String(this.sqlite.lowLevel.columnDouble(stmt, i))
              break
            case 'text':
              value = this.sqlite.lowLevel.columnText(stmt, i)
              break
            case 'blob':
              value = `<blob:${this.sqlite.lowLevel.columnBlob(stmt, i).length}>`
              break
            default:
              value = '?'
          }
          row.push(value)
        }
        rows.push(row)
        stepResult = this.sqlite.lowLevel.step(stmt)
      }

      this.sqlite.lowLevel.finalize(stmt)

      // Display results
      if (columns.length > 0 && rows.length > 0) {
        this.displayTable(columns, rows)
      } else if (stepResult === 'done' && columns.length === 0) {
        this.terminal.writeln('\x1b[32mQuery executed successfully\x1b[0m')
      } else if (rows.length === 0) {
        this.terminal.writeln('\x1b[90m(no results)\x1b[0m')
      }

    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.terminal.writeln(`\x1b[31mError: ${msg}\x1b[0m`)
    }
  }

  private displayTable(columns: string[], rows: string[][]) {
    // Calculate column widths
    const widths = columns.map((col, i) => {
      const maxRowWidth = Math.max(...rows.map(row => (row[i] || '').length))
      return Math.max(col.length, maxRowWidth, 4)
    })

    // Header
    const header = columns.map((col, i) => col.padEnd(widths[i])).join(' | ')
    const separator = widths.map(w => '-'.repeat(w)).join('-+-')

    this.terminal.writeln('\x1b[1m' + header + '\x1b[0m')
    this.terminal.writeln(separator)

    // Rows
    for (const row of rows) {
      const line = row.map((cell, i) => (cell || '').padEnd(widths[i])).join(' | ')
      this.terminal.writeln(line)
    }

    this.terminal.writeln(`\x1b[90m(${rows.length} row${rows.length !== 1 ? 's' : ''})\x1b[0m`)
  }

  // ============================================================================
  // Data Loading Methods
  // ============================================================================

  async loadSql(sql: string, source: string = 'input'): Promise<void> {
    this.terminal.writeln(`\x1b[90mExecuting SQL from ${source}...\x1b[0m`)

    // Split into individual statements
    const statements = sql
      .split(/;(?=(?:[^']*'[^']*')*[^']*$)/)
      .map(s => s.trim())
      .filter(s => s.length > 0 && !s.startsWith('--'))

    let executed = 0
    let errors = 0

    for (const stmt of statements) {
      try {
        const preparedStmt = this.sqlite.lowLevel.prepare(this.db, stmt + ';')
        while (this.sqlite.lowLevel.step(preparedStmt) === 'row') {
          // Execute all rows
        }
        this.sqlite.lowLevel.finalize(preparedStmt)
        executed++
      } catch (e) {
        errors++
        const msg = e instanceof Error ? e.message : String(e)
        this.terminal.writeln(`\x1b[31mError in statement: ${msg}\x1b[0m`)
        this.terminal.writeln(`\x1b[90m  ${stmt.substring(0, 60)}${stmt.length > 60 ? '...' : ''}\x1b[0m`)
      }
    }

    this.terminal.writeln(`\x1b[32mExecuted ${executed} statements${errors > 0 ? `, ${errors} errors` : ''}\x1b[0m`)
  }

  async loadCsv(csv: string, tableName: string): Promise<void> {
    this.terminal.writeln(`\x1b[90mImporting CSV into table '${tableName}'...\x1b[0m`)

    const lines = csv.split(/\r?\n/).filter(l => l.trim().length > 0)
    if (lines.length < 2) {
      this.terminal.writeln('\x1b[31mCSV must have header row and at least one data row\x1b[0m')
      return
    }

    // Parse header
    const headers = this.parseCsvLine(lines[0])
    const columns = headers.map(h => h.replace(/[^a-zA-Z0-9_]/g, '_'))

    // Create table
    const createSql = `CREATE TABLE IF NOT EXISTS ${tableName} (${columns.map(c => `${c} TEXT`).join(', ')});`
    try {
      const stmt = this.sqlite.lowLevel.prepare(this.db, createSql)
      this.sqlite.lowLevel.step(stmt)
      this.sqlite.lowLevel.finalize(stmt)
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.terminal.writeln(`\x1b[31mError creating table: ${msg}\x1b[0m`)
      return
    }

    // Insert data
    let inserted = 0
    for (let i = 1; i < lines.length; i++) {
      const values = this.parseCsvLine(lines[i])
      if (values.length !== columns.length) continue

      const escapedValues = values.map(v => `'${v.replace(/'/g, "''")}'`)
      const insertSql = `INSERT INTO ${tableName} (${columns.join(', ')}) VALUES (${escapedValues.join(', ')});`

      try {
        const stmt = this.sqlite.lowLevel.prepare(this.db, insertSql)
        this.sqlite.lowLevel.step(stmt)
        this.sqlite.lowLevel.finalize(stmt)
        inserted++
      } catch (e) {
        // Skip row on error
      }
    }

    this.terminal.writeln(`\x1b[32mImported ${inserted} rows into '${tableName}'\x1b[0m`)
  }

  private parseCsvLine(line: string): string[] {
    const result: string[] = []
    let current = ''
    let inQuotes = false

    for (let i = 0; i < line.length; i++) {
      const char = line[i]

      if (char === '"') {
        if (inQuotes && line[i + 1] === '"') {
          current += '"'
          i++
        } else {
          inQuotes = !inQuotes
        }
      } else if (char === ',' && !inQuotes) {
        result.push(current.trim())
        current = ''
      } else {
        current += char
      }
    }

    result.push(current.trim())
    return result
  }

  async loadFromUrl(url: string): Promise<void> {
    this.terminal.writeln(`\x1b[90mFetching ${url}...\x1b[0m`)

    try {
      const response = await fetch(url)
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}: ${response.statusText}`)
      }

      const content = await response.text()
      const isCSV = url.endsWith('.csv') || response.headers.get('content-type')?.includes('csv')

      if (isCSV) {
        const tableName = url.split('/').pop()?.replace(/\.[^.]+$/, '') || 'imported'
        await this.loadCsv(content, tableName)
      } else {
        await this.loadSql(content, url)
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.terminal.writeln(`\x1b[31mError fetching URL: ${msg}\x1b[0m`)
    }
  }

  async loadDemo(): Promise<void> {
    await this.loadFromUrl('/demo.sql')
  }

  exportSql(): string {
    const tables: string[] = []
    const data: string[] = []

    // Get table schemas
    try {
      const stmt = this.sqlite.lowLevel.prepare(
        this.db,
        "SELECT name, sql FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name;"
      )

      while (this.sqlite.lowLevel.step(stmt) === 'row') {
        const name = this.sqlite.lowLevel.columnText(stmt, 0)
        const sql = this.sqlite.lowLevel.columnText(stmt, 1)
        tables.push(sql + ';')

        // Get table data
        const dataStmt = this.sqlite.lowLevel.prepare(this.db, `SELECT * FROM ${name};`)
        const colCount = this.sqlite.lowLevel.columnCount(dataStmt)
        const columns: string[] = []
        for (let i = 0; i < colCount; i++) {
          columns.push(this.sqlite.lowLevel.columnName(dataStmt, i))
        }

        while (this.sqlite.lowLevel.step(dataStmt) === 'row') {
          const values: string[] = []
          for (let i = 0; i < colCount; i++) {
            const type = this.sqlite.lowLevel.getColumnType(dataStmt, i)
            switch (type) {
              case 'null':
                values.push('NULL')
                break
              case 'integer':
                values.push(String(this.sqlite.lowLevel.columnInt64(dataStmt, i)))
                break
              case 'float':
                values.push(String(this.sqlite.lowLevel.columnDouble(dataStmt, i)))
                break
              case 'text':
                values.push(`'${this.sqlite.lowLevel.columnText(dataStmt, i).replace(/'/g, "''")}'`)
                break
              case 'blob':
                values.push("X'" + Array.from(this.sqlite.lowLevel.columnBlob(dataStmt, i)).map(b => b.toString(16).padStart(2, '0')).join('') + "'")
                break
            }
          }
          data.push(`INSERT INTO ${name} (${columns.join(', ')}) VALUES (${values.join(', ')});`)
        }
        this.sqlite.lowLevel.finalize(dataStmt)
      }
      this.sqlite.lowLevel.finalize(stmt)

      // Get views
      const viewStmt = this.sqlite.lowLevel.prepare(
        this.db,
        "SELECT sql FROM sqlite_master WHERE type='view' ORDER BY name;"
      )
      while (this.sqlite.lowLevel.step(viewStmt) === 'row') {
        const sql = this.sqlite.lowLevel.columnText(viewStmt, 0)
        if (sql) tables.push(sql + ';')
      }
      this.sqlite.lowLevel.finalize(viewStmt)

      // Get indexes
      const indexStmt = this.sqlite.lowLevel.prepare(
        this.db,
        "SELECT sql FROM sqlite_master WHERE type='index' AND sql IS NOT NULL ORDER BY name;"
      )
      while (this.sqlite.lowLevel.step(indexStmt) === 'row') {
        const sql = this.sqlite.lowLevel.columnText(indexStmt, 0)
        if (sql) tables.push(sql + ';')
      }
      this.sqlite.lowLevel.finalize(indexStmt)

    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.terminal.writeln(`\x1b[31mError exporting: ${msg}\x1b[0m`)
      return ''
    }

    return '-- SQLite WASM Export\n-- Generated: ' + new Date().toISOString() + '\n\n' +
           tables.join('\n\n') + '\n\n' + data.join('\n')
  }

  async saveToStorage(): Promise<boolean> {
    if (!await hasOPFS()) {
      this.terminal.writeln('\x1b[31mOPFS not supported in this browser\x1b[0m')
      return false
    }

    this.terminal.writeln('\x1b[90mSaving to browser storage...\x1b[0m')

    try {
      const sql = this.exportSql()
      const encoder = new TextEncoder()
      const data = encoder.encode(sql)
      await saveToOPFS(data)
      this.terminal.writeln('\x1b[32mDatabase saved to browser storage\x1b[0m')
      this.updateStorageStatus()
      return true
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.terminal.writeln(`\x1b[31mError saving: ${msg}\x1b[0m`)
      return false
    }
  }

  async loadFromStorage(): Promise<boolean> {
    if (!await hasOPFS()) {
      this.terminal.writeln('\x1b[31mOPFS not supported in this browser\x1b[0m')
      return false
    }

    this.terminal.writeln('\x1b[90mLoading from browser storage...\x1b[0m')

    try {
      const data = await loadFromOPFS()
      if (!data) {
        this.terminal.writeln('\x1b[33mNo saved database found\x1b[0m')
        return false
      }

      const decoder = new TextDecoder()
      const sql = decoder.decode(data)
      await this.loadSql(sql, 'browser storage')
      return true
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.terminal.writeln(`\x1b[31mError loading: ${msg}\x1b[0m`)
      return false
    }
  }

  private async updateStorageStatus(): Promise<void> {
    if (await hasOPFS()) {
      const data = await loadFromOPFS()
      if (data) {
        const kb = (data.length / 1024).toFixed(1)
        storageStatus.textContent = `Saved: ${kb} KB`
      } else {
        storageStatus.textContent = ''
      }
    }
  }

  async handleFile(file: File): Promise<void> {
    const content = await file.text()
    const ext = file.name.split('.').pop()?.toLowerCase()

    if (ext === 'csv') {
      const tableName = file.name.replace(/\.[^.]+$/, '').replace(/[^a-zA-Z0-9_]/g, '_')
      await this.loadCsv(content, tableName)
    } else {
      await this.loadSql(content, file.name)
    }

    this.showPrompt()
  }

  // ============================================================================
  // WASM Extension Loading
  // ============================================================================

  async loadWasmExtension(url: string): Promise<void> {
    this.terminal.writeln(`\x1b[90mLoading WASM extension from ${url}...\x1b[0m`)

    try {
      const ext = await extensionCallbacks.loadWasmExtension(url)
      this.terminal.writeln(`\x1b[32mLoaded extension: ${ext.name} v${ext.version}\x1b[0m`)
      if (ext.functions.length > 0) {
        this.terminal.writeln(`\x1b[90mExported functions: ${ext.functions.join(', ')}\x1b[0m`)
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.terminal.writeln(`\x1b[31mError loading extension: ${msg}\x1b[0m`)
    }
  }
}

async function main() {
  try {
    updateLoadingText('Initializing terminal...')

    const terminal = new Terminal({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: 'Menlo, Monaco, "Courier New", monospace',
      theme: {
        background: '#0f0f23',
        foreground: '#cccccc',
        cursor: '#e94560',
        cursorAccent: '#0f0f23',
        selectionBackground: '#264f78',
      },
    })

    const fitAddon = new FitAddon()
    terminal.loadAddon(fitAddon)

    loadingEl.style.display = 'none'
    terminal.open(containerEl)
    fitAddon.fit()

    window.addEventListener('resize', () => fitAddon.fit())

    terminal.writeln('\x1b[90mLoading SQLite WebAssembly...\x1b[0m')

    // Set up xterm stdio
    const stdioProvider = createXtermStdio(terminal)
    setGlobalStdioProvider(stdioProvider)

    await registerCorePlugins()
    for (const plugin of socketPlugins) {
      globalRegistry.register(plugin)
    }

    const polyfill = new Polyfill({ policy: new AllowAllPolicy() })

    const { imports } = await polyfill.forInterfaces([
      'wasi:cli/exit@0.2.0',
      'wasi:cli/stdin@0.2.0',
      'wasi:cli/stdout@0.2.0',
      'wasi:cli/stderr@0.2.0',
      'wasi:cli/terminal-input@0.2.0',
      'wasi:cli/terminal-output@0.2.0',
      'wasi:cli/terminal-stdin@0.2.0',
      'wasi:cli/terminal-stdout@0.2.0',
      'wasi:cli/terminal-stderr@0.2.0',
      'wasi:clocks/monotonic-clock@0.2.0',
      'wasi:clocks/wall-clock@0.2.0',
      'wasi:filesystem/types@0.2.0',
      'wasi:filesystem/preopens@0.2.0',
      'wasi:io/error@0.2.0',
      'wasi:io/poll@0.2.0',
      'wasi:io/streams@0.2.0',
      'wasi:sockets/tcp@0.2.0',
      'wasi:sockets/udp@0.2.0',
    ])

    // Transform imports to remove version suffixes (jco transpiled code expects unversioned keys)
    const unversionedImports: Record<string, unknown> = {}
    for (const [key, value] of Object.entries(imports)) {
      // Remove version suffix like @0.2.0
      const unversionedKey = key.replace(/@[\d.]+$/, '')
      unversionedImports[unversionedKey] = value
    }

    // Add extension callbacks
    const fullImports = {
      ...unversionedImports,
      'sqlite:wasm/extension-callbacks': extensionCallbacks,
    } as unknown as ImportObject

    terminal.writeln('\x1b[90mInstantiating SQLite...\x1b[0m')

    const sqlite = await instantiate(getCoreModule, fullImports)

    terminal.clear()

    // Create and start REPL
    const repl = new SqliteRepl(terminal, sqlite)
    await repl.init()

    // Setup file upload button
    btnUpload.addEventListener('click', () => fileInput.click())

    fileInput.addEventListener('change', async () => {
      const file = fileInput.files?.[0]
      if (file) {
        await repl.handleFile(file)
        fileInput.value = ''
      }
    })

    // Setup demo button
    btnDemo.addEventListener('click', async () => {
      await repl.loadDemo()
      repl['showPrompt']()
    })

    // Setup export button
    btnExport.addEventListener('click', () => {
      const sql = repl.exportSql()
      if (sql) {
        const blob = new Blob([sql], { type: 'application/sql' })
        const url = URL.createObjectURL(blob)
        const a = document.createElement('a')
        a.href = url
        a.download = 'export.sql'
        a.click()
        URL.revokeObjectURL(url)
        terminal.writeln('\x1b[32mDatabase exported to export.sql\x1b[0m')
        repl['showPrompt']()
      }
    })

    // Setup OPFS save/load buttons
    btnSave.addEventListener('click', async () => {
      await repl.saveToStorage()
      repl['showPrompt']()
    })

    btnLoad.addEventListener('click', async () => {
      await repl.loadFromStorage()
      repl['showPrompt']()
    })

    // Setup drag and drop
    containerEl.addEventListener('dragover', (e) => {
      e.preventDefault()
      e.stopPropagation()
      containerEl.classList.add('drag-over')
    })

    containerEl.addEventListener('dragleave', (e) => {
      e.preventDefault()
      e.stopPropagation()
      containerEl.classList.remove('drag-over')
    })

    containerEl.addEventListener('drop', async (e) => {
      e.preventDefault()
      e.stopPropagation()
      containerEl.classList.remove('drag-over')

      const file = e.dataTransfer?.files[0]
      if (file) {
        await repl.handleFile(file)
      }
    })

    // Check for saved data on startup
    if (await hasOPFS()) {
      const data = await loadFromOPFS()
      if (data) {
        const kb = (data.length / 1024).toFixed(1)
        storageStatus.textContent = `Saved: ${kb} KB`
      }
    }

  } catch (err) {
    console.error('Error:', err)
    showError(err instanceof Error ? err.message : String(err))
  }
}

main()
