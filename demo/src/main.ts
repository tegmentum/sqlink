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
    this.terminal.writeln('\x1b[36mAvailable extension manager commands:\x1b[0m')
    this.terminal.writeln('  SELECT wasm_search(\'text\');     -- Search extensions')
    this.terminal.writeln('  SELECT wasm_list();             -- List installed')
    this.terminal.writeln('  SELECT wasm_info(\'text\');       -- Extension info')
    this.terminal.writeln('  SELECT wasm_install(\'text\');    -- Install extension')
    this.terminal.writeln('')
    this.terminal.writeln('\x1b[36mBuilt-in functions:\x1b[0m')
    this.terminal.writeln('  uuid(), reverse(), math_sqrt(), regexp()')
    this.terminal.writeln('  text_upper(), text_lower(), repeat(), char_length()')
    this.terminal.writeln('')
    this.terminal.writeln('Type \x1b[33m.help\x1b[0m for more commands, \x1b[33m.quit\x1b[0m to exit')
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

  private handleEnter() {
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
      this.handleMetaCommand(input)
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

  private handleMetaCommand(cmd: string) {
    const parts = cmd.split(/\s+/)
    const command = parts[0].toLowerCase()

    switch (command) {
      case '.help':
        this.terminal.writeln('\x1b[36mMeta Commands:\x1b[0m')
        this.terminal.writeln('  .help              Show this help')
        this.terminal.writeln('  .tables            List tables')
        this.terminal.writeln('  .schema [table]    Show schema')
        this.terminal.writeln('  .quit              Exit')
        this.terminal.writeln('  .clear             Clear screen')
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

      case '.quit':
      case '.exit':
        this.terminal.writeln('\x1b[90mGoodbye!\x1b[0m')
        this.sqlite.lowLevel.close(this.db)
        break

      case '.clear':
        this.terminal.clear()
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

  } catch (err) {
    console.error('Error:', err)
    showError(err instanceof Error ? err.message : String(err))
  }
}

main()
