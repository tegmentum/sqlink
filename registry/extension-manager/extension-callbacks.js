/**
 * Extension Manager Callbacks
 * Provides SQL interface for managing SQLite WASM extensions
 *
 * This extension creates and manages internal tables for tracking
 * extension registry and installation state.
 *
 * Tables created:
 *   _wasm_extensions      - Cached extension registry
 *   _wasm_installed       - Installed extensions
 *   _wasm_registry_meta   - Registry metadata (sync time, etc.)
 */

// ============================================================================
// Function ID constants (200+)
// ============================================================================
export const FUNC_WASM_SYNC = 200n;
export const FUNC_WASM_SEARCH = 201n;
export const FUNC_WASM_LIST = 202n;
export const FUNC_WASM_INSTALL = 203n;
export const FUNC_WASM_UNINSTALL = 204n;
export const FUNC_WASM_INFO = 205n;
export const FUNC_WASM_UPDATE = 206n;
export const FUNC_WASM_REGISTRY_VERSION = 207n;
export const FUNC_WASM_INIT = 208n;

// ============================================================================
// Extension state - managed by the host environment
// ============================================================================
let extensionRegistry = null;
let installedExtensions = new Map();
let registryVersion = '1.0.0';
let lastSync = null;
let dbContext = null;

/**
 * Initialize the extension manager with a database context and registry data
 * Called by the host environment when loading the extension
 */
export function initializeExtensionManager(context) {
    if (context.registry) {
        extensionRegistry = context.registry;
        registryVersion = context.registry.version || '1.0.0';
    }
    if (context.installed) {
        installedExtensions = new Map(Object.entries(context.installed));
    }
    if (context.lastSync) {
        lastSync = context.lastSync;
    }
    dbContext = context.db || null;
}

/**
 * Get current state for persistence
 */
export function getExtensionManagerState() {
    return {
        registry: extensionRegistry,
        installed: Object.fromEntries(installedExtensions),
        lastSync: lastSync,
        registryVersion: registryVersion
    };
}

/**
 * Update installed extensions from host
 */
export function markExtensionInstalled(name, version, installedAt) {
    installedExtensions.set(name, {
        version: version,
        installedAt: installedAt || new Date().toISOString()
    });
}

/**
 * Update installed extensions from host
 */
export function markExtensionUninstalled(name) {
    installedExtensions.delete(name);
}

/**
 * Update registry from host
 */
export function updateRegistry(registry) {
    extensionRegistry = registry;
    registryVersion = registry.version || '1.0.0';
    lastSync = new Date().toISOString();
}

// ============================================================================
// Helper functions
// ============================================================================
function makeNull() {
    return { valueType: 'null' };
}

function makeInteger(val) {
    return { valueType: 'integer', intValue: BigInt(val) };
}

function makeFloat(val) {
    return { valueType: 'float', floatValue: val };
}

function makeText(val) {
    return { valueType: 'text', textValue: String(val) };
}

function makeBlob(val) {
    return { valueType: 'blob', blobValue: val };
}

function getValue(sqlValue) {
    switch (sqlValue.valueType) {
        case 'integer': return Number(sqlValue.intValue);
        case 'float': return sqlValue.floatValue;
        case 'text': return sqlValue.textValue;
        case 'blob': return sqlValue.blobValue;
        case 'null':
        default: return null;
    }
}

// ============================================================================
// Scalar function dispatcher
// ============================================================================
export function onScalarFunction(functionId, args) {
    switch (functionId) {
        case FUNC_WASM_REGISTRY_VERSION: {
            // SELECT wasm_registry_version();
            // Returns the current registry version
            return makeText(registryVersion);
        }

        case FUNC_WASM_SYNC: {
            // SELECT wasm_sync();
            // Returns JSON with sync result
            // Actual sync is handled by host - this just reports state
            if (!extensionRegistry) {
                return makeText(JSON.stringify({
                    success: false,
                    error: 'No registry loaded',
                    extensions_count: 0,
                    last_sync: null
                }));
            }
            const extCount = extensionRegistry.extensions ? extensionRegistry.extensions.length : 0;
            return makeText(JSON.stringify({
                success: true,
                extensions_count: extCount,
                last_sync: lastSync,
                registry_version: registryVersion
            }));
        }

        case FUNC_WASM_SEARCH: {
            // SELECT wasm_search('text');
            // Returns JSON array of matching extensions
            if (args.length < 1) {
                throw new Error('wasm_search requires 1 argument (query)');
            }
            const query = getValue(args[0]);
            if (query === null) {
                return makeText('[]');
            }

            if (!extensionRegistry || !extensionRegistry.extensions) {
                return makeText('[]');
            }

            const queryLower = String(query).toLowerCase();
            const results = extensionRegistry.extensions.filter(ext => {
                const nameMatch = ext.name && ext.name.toLowerCase().includes(queryLower);
                const descMatch = ext.description && ext.description.toLowerCase().includes(queryLower);
                const keywordMatch = ext.keywords && ext.keywords.some(k =>
                    k.toLowerCase().includes(queryLower)
                );
                return nameMatch || descMatch || keywordMatch;
            }).map(ext => ({
                name: ext.name,
                version: ext.version,
                description: ext.description || '',
                installed: installedExtensions.has(ext.name),
                installed_version: installedExtensions.get(ext.name)?.version || null
            }));

            return makeText(JSON.stringify(results));
        }

        case FUNC_WASM_LIST: {
            // SELECT wasm_list();
            // Returns JSON array of installed extensions
            const results = [];
            for (const [name, info] of installedExtensions) {
                const registryExt = extensionRegistry?.extensions?.find(e => e.name === name);
                results.push({
                    name: name,
                    version: info.version,
                    installed_at: info.installedAt,
                    update_available: registryExt ? registryExt.version !== info.version : false,
                    latest_version: registryExt?.version || info.version
                });
            }
            return makeText(JSON.stringify(results));
        }

        case FUNC_WASM_INSTALL: {
            // SELECT wasm_install('text');
            // SELECT wasm_install('text', '0.1.0');
            // Returns JSON with install status
            // Note: Actual installation is handled by host - this marks intent
            if (args.length < 1) {
                throw new Error('wasm_install requires extension name');
            }
            const name = getValue(args[0]);
            const requestedVersion = args.length > 1 ? getValue(args[1]) : null;

            if (!name) {
                return makeText(JSON.stringify({
                    success: false,
                    error: 'Extension name required'
                }));
            }

            if (!extensionRegistry || !extensionRegistry.extensions) {
                return makeText(JSON.stringify({
                    success: false,
                    error: 'Registry not loaded. Run wasm_sync() first.'
                }));
            }

            const ext = extensionRegistry.extensions.find(e => e.name === name);
            if (!ext) {
                return makeText(JSON.stringify({
                    success: false,
                    error: `Extension '${name}' not found in registry`
                }));
            }

            if (installedExtensions.has(name)) {
                const installed = installedExtensions.get(name);
                return makeText(JSON.stringify({
                    success: false,
                    error: `Extension '${name}' already installed (version ${installed.version})`,
                    hint: 'Use wasm_update() to update to a new version'
                }));
            }

            const version = requestedVersion || ext.version;

            // Return install intent - host environment handles actual download
            return makeText(JSON.stringify({
                success: true,
                action: 'install',
                name: name,
                version: version,
                oci_artifact: ext.oci_artifact,
                checksum: ext.checksum,
                exports: ext.exports || [],
                message: `Extension '${name}' v${version} queued for installation`
            }));
        }

        case FUNC_WASM_UNINSTALL: {
            // SELECT wasm_uninstall('text');
            // Returns JSON with uninstall status
            if (args.length < 1) {
                throw new Error('wasm_uninstall requires extension name');
            }
            const name = getValue(args[0]);

            if (!name) {
                return makeText(JSON.stringify({
                    success: false,
                    error: 'Extension name required'
                }));
            }

            if (!installedExtensions.has(name)) {
                return makeText(JSON.stringify({
                    success: false,
                    error: `Extension '${name}' is not installed`
                }));
            }

            const installed = installedExtensions.get(name);

            // Return uninstall intent - host environment handles actual removal
            return makeText(JSON.stringify({
                success: true,
                action: 'uninstall',
                name: name,
                version: installed.version,
                message: `Extension '${name}' queued for uninstallation`
            }));
        }

        case FUNC_WASM_INFO: {
            // SELECT wasm_info('text');
            // Returns JSON with detailed extension info
            if (args.length < 1) {
                throw new Error('wasm_info requires extension name');
            }
            const name = getValue(args[0]);

            if (!name) {
                return makeNull();
            }

            if (!extensionRegistry || !extensionRegistry.extensions) {
                return makeText(JSON.stringify({
                    error: 'Registry not loaded'
                }));
            }

            const ext = extensionRegistry.extensions.find(e => e.name === name);
            if (!ext) {
                return makeText(JSON.stringify({
                    error: `Extension '${name}' not found`
                }));
            }

            const installed = installedExtensions.get(name);

            return makeText(JSON.stringify({
                name: ext.name,
                version: ext.version,
                description: ext.description || '',
                license: ext.license || 'Unknown',
                authors: ext.authors || [],
                repository: ext.repository || '',
                homepage: ext.homepage || '',
                keywords: ext.keywords || [],
                categories: ext.categories || [],
                exports: ext.exports || [],
                min_sqlite_version: ext.min_sqlite_version || '',
                oci_artifact: ext.oci_artifact || '',
                checksum: ext.checksum || '',
                installed: !!installed,
                installed_version: installed?.version || null,
                installed_at: installed?.installedAt || null,
                update_available: installed ? ext.version !== installed.version : false
            }));
        }

        case FUNC_WASM_UPDATE: {
            // SELECT wasm_update('text');
            // SELECT wasm_update(); -- update all
            // Returns JSON with update status
            const name = args.length > 0 ? getValue(args[0]) : null;

            if (!extensionRegistry || !extensionRegistry.extensions) {
                return makeText(JSON.stringify({
                    success: false,
                    error: 'Registry not loaded. Run wasm_sync() first.'
                }));
            }

            const updates = [];

            if (name) {
                // Update specific extension
                if (!installedExtensions.has(name)) {
                    return makeText(JSON.stringify({
                        success: false,
                        error: `Extension '${name}' is not installed`
                    }));
                }

                const ext = extensionRegistry.extensions.find(e => e.name === name);
                if (!ext) {
                    return makeText(JSON.stringify({
                        success: false,
                        error: `Extension '${name}' not found in registry`
                    }));
                }

                const installed = installedExtensions.get(name);
                if (ext.version === installed.version) {
                    return makeText(JSON.stringify({
                        success: true,
                        message: `Extension '${name}' is already at the latest version (${ext.version})`,
                        updates: []
                    }));
                }

                updates.push({
                    name: name,
                    old_version: installed.version,
                    new_version: ext.version,
                    oci_artifact: ext.oci_artifact,
                    checksum: ext.checksum
                });
            } else {
                // Update all extensions
                for (const [extName, installed] of installedExtensions) {
                    const ext = extensionRegistry.extensions.find(e => e.name === extName);
                    if (ext && ext.version !== installed.version) {
                        updates.push({
                            name: extName,
                            old_version: installed.version,
                            new_version: ext.version,
                            oci_artifact: ext.oci_artifact,
                            checksum: ext.checksum
                        });
                    }
                }
            }

            return makeText(JSON.stringify({
                success: true,
                action: 'update',
                updates: updates,
                message: updates.length > 0
                    ? `${updates.length} extension(s) queued for update`
                    : 'All extensions are up to date'
            }));
        }

        case FUNC_WASM_INIT: {
            // SELECT wasm_init();
            // Initialize extension manager tables (for persistence)
            // Returns SQL statements to create tables
            const sql = `
CREATE TABLE IF NOT EXISTS _wasm_extensions (
    name TEXT PRIMARY KEY,
    version TEXT NOT NULL,
    description TEXT,
    license TEXT,
    repository TEXT,
    homepage TEXT,
    keywords TEXT,
    categories TEXT,
    exports TEXT,
    min_sqlite_version TEXT,
    oci_artifact TEXT,
    checksum TEXT,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS _wasm_installed (
    name TEXT PRIMARY KEY,
    version TEXT NOT NULL,
    installed_at TEXT DEFAULT CURRENT_TIMESTAMP,
    path TEXT,
    FOREIGN KEY (name) REFERENCES _wasm_extensions(name)
);

CREATE TABLE IF NOT EXISTS _wasm_registry_meta (
    key TEXT PRIMARY KEY,
    value TEXT
);

INSERT OR REPLACE INTO _wasm_registry_meta (key, value) VALUES ('version', '${registryVersion}');
INSERT OR REPLACE INTO _wasm_registry_meta (key, value) VALUES ('initialized', datetime('now'));
`;
            return makeText(sql);
        }

        default:
            throw new Error(`Unknown function id: ${functionId}`);
    }
}

// ============================================================================
// Aggregate function dispatchers (not used by extension manager)
// ============================================================================
export function onAggregateStep(functionId, contextId, args) {
    throw new Error(`Extension manager does not have aggregate functions`);
}

export function onAggregateFinalize(functionId, contextId) {
    throw new Error(`Extension manager does not have aggregate functions`);
}

// ============================================================================
// Collation dispatcher (not used by extension manager)
// ============================================================================
export function onCollationCompare(collationId, a, b) {
    if (a < b) return -1;
    if (a > b) return 1;
    return 0;
}

// ============================================================================
// Hook callbacks (not used by extension manager)
// ============================================================================
export function onUpdate(hookId, op, database, table, rowid) {}
export function onCommit(hookId) { return false; }
export function onRollback(hookId) {}
export function onAuthorize(authId, action, arg1, arg2, database, trigger) {
    return 'ok';
}
