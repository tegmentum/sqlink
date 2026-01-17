/**
 * Register extension-manager functions
 *
 * Usage:
 *   import { registerExtensionManager } from './register.mjs';
 *   import { lowLevel, extension } from 'sqlite-extensible.js';
 *   import registry from './registry/index.json';
 *
 *   const db = lowLevel.open(':memory:', { readwrite: true, create: true, memory: true });
 *   registerExtensionManager(db, extension, { registry });
 */

import * as callbacks from './extension-callbacks.js';

/**
 * Register all extension-manager functions with a database handle
 * @param {bigint} db - Database handle from lowLevel.open()
 * @param {object} extension - Extension API from sqlite-extensible
 * @param {object} options - Configuration options
 * @param {object} options.registry - Registry data (from index.json)
 * @param {object} options.installed - Map of installed extensions
 * @returns {bigint[]} Array of function handles (for cleanup if needed)
 */
export function registerExtensionManager(db, extension, options = {}) {
    const handles = [];

    // Initialize the extension manager with context
    callbacks.initializeExtensionManager({
        registry: options.registry || null,
        installed: options.installed || {},
        lastSync: options.lastSync || null,
        db: db
    });

    // wasm_registry_version() - Get registry version
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_registry_version',
        0,
        { deterministic: true },
        callbacks.FUNC_WASM_REGISTRY_VERSION
    ));

    // wasm_sync() - Report sync status
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_sync',
        0,
        { deterministic: false },
        callbacks.FUNC_WASM_SYNC
    ));

    // wasm_search(query) - Search extensions by name/description/keywords
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_search',
        1,
        { deterministic: false },
        callbacks.FUNC_WASM_SEARCH
    ));

    // wasm_list() - List installed extensions
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_list',
        0,
        { deterministic: false },
        callbacks.FUNC_WASM_LIST
    ));

    // wasm_install(name) - Install extension by name
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_install',
        1,
        { deterministic: false },
        callbacks.FUNC_WASM_INSTALL
    ));

    // wasm_install(name, version) - Install specific version
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_install',
        2,
        { deterministic: false },
        callbacks.FUNC_WASM_INSTALL
    ));

    // wasm_uninstall(name) - Uninstall extension
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_uninstall',
        1,
        { deterministic: false },
        callbacks.FUNC_WASM_UNINSTALL
    ));

    // wasm_info(name) - Get detailed extension info
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_info',
        1,
        { deterministic: false },
        callbacks.FUNC_WASM_INFO
    ));

    // wasm_update() - Update all extensions
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_update',
        0,
        { deterministic: false },
        callbacks.FUNC_WASM_UPDATE
    ));

    // wasm_update(name) - Update specific extension
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_update',
        1,
        { deterministic: false },
        callbacks.FUNC_WASM_UPDATE
    ));

    // wasm_init() - Get SQL to create management tables
    handles.push(extension.registerScalarFunction(
        db,
        'wasm_init',
        0,
        { deterministic: true },
        callbacks.FUNC_WASM_INIT
    ));

    return handles;
}

/**
 * Update the extension manager with new registry data
 * @param {object} registry - New registry data
 */
export function updateExtensionRegistry(registry) {
    callbacks.updateRegistry(registry);
}

/**
 * Mark an extension as installed (called after successful installation)
 * @param {string} name - Extension name
 * @param {string} version - Installed version
 * @param {string} installedAt - Installation timestamp (optional)
 */
export function markInstalled(name, version, installedAt) {
    callbacks.markExtensionInstalled(name, version, installedAt);
}

/**
 * Mark an extension as uninstalled
 * @param {string} name - Extension name
 */
export function markUninstalled(name) {
    callbacks.markExtensionUninstalled(name);
}

/**
 * Get current extension manager state for persistence
 * @returns {object} State object with registry, installed, lastSync
 */
export function getState() {
    return callbacks.getExtensionManagerState();
}

/**
 * Unregister all functions (cleanup)
 * @param {object} extension - Extension API
 * @param {bigint[]} handles - Array of function handles from registerExtensionManager
 */
export function unregisterExtensionManager(extension, handles) {
    for (const handle of handles) {
        try {
            extension.unregisterFunction(handle);
        } catch (e) {
            console.warn(`Failed to unregister function: ${e.message}`);
        }
    }
}

// Export function IDs for advanced usage
export {
    FUNC_WASM_SYNC,
    FUNC_WASM_SEARCH,
    FUNC_WASM_LIST,
    FUNC_WASM_INSTALL,
    FUNC_WASM_UNINSTALL,
    FUNC_WASM_INFO,
    FUNC_WASM_UPDATE,
    FUNC_WASM_REGISTRY_VERSION,
    FUNC_WASM_INIT
} from './extension-callbacks.js';
