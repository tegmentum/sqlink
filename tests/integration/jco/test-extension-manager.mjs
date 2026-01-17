/**
 * SQLite WASM Extension Manager Tests
 * Tests SQL-based extension management functions
 */

import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import { readFileSync } from 'fs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// Test state
let passed = 0;
let failed = 0;

function assert(condition, message) {
    if (condition) {
        passed++;
        console.log(`  ✓ ${message}`);
    } else {
        failed++;
        console.log(`  ✗ ${message}`);
    }
}

function assertEqual(actual, expected, message) {
    if (actual === expected) {
        passed++;
        console.log(`  ✓ ${message}`);
    } else {
        failed++;
        console.log(`  ✗ ${message}`);
        console.log(`    Expected: ${expected}`);
        console.log(`    Actual: ${actual}`);
    }
}

function assertContains(actual, substring, message) {
    if (actual && actual.includes(substring)) {
        passed++;
        console.log(`  ✓ ${message}`);
    } else {
        failed++;
        console.log(`  ✗ ${message}`);
        console.log(`    Expected to contain: ${substring}`);
        console.log(`    Actual: ${actual}`);
    }
}

async function runTests() {
    console.log('SQLite WASM Extension Manager Tests\n');

    // Load registry data
    const registryPath = join(__dirname, '../../../registry/index.json');
    const registry = JSON.parse(readFileSync(registryPath, 'utf-8'));

    // Import the extension callbacks
    const callbacks = await import(join(__dirname, '../../../build/js-ext/extension-callbacks.js'));
    const {
        FUNC_WASM_SYNC,
        FUNC_WASM_SEARCH,
        FUNC_WASM_LIST,
        FUNC_WASM_INSTALL,
        FUNC_WASM_UNINSTALL,
        FUNC_WASM_INFO,
        FUNC_WASM_UPDATE,
        FUNC_WASM_REGISTRY_VERSION,
        FUNC_WASM_INIT,
        initializeExtensionManager,
        markExtensionInstalled,
        markExtensionUninstalled,
        getExtensionManagerState
    } = callbacks;

    // Import the extensible SQLite component
    const sqlite = await import(join(__dirname, '../../../build/js-ext/sqlite-extensible.js'));
    const { lowLevel, extension } = sqlite;

    // Open database
    const openFlags = { readwrite: true, create: true, memory: true };
    const db = lowLevel.open(':memory:', openFlags);
    assert(db !== 0n, 'Database opened successfully');

    // Initialize extension manager with registry
    console.log('\n1. Initializing Extension Manager\n');

    initializeExtensionManager({
        registry: registry,
        installed: {},
        lastSync: new Date().toISOString()
    });
    assert(true, 'Extension manager initialized with registry');

    // Register extension manager functions
    console.log('\n2. Registering Extension Manager Functions\n');

    const handles = [];

    try {
        // wasm_registry_version()
        handles.push(extension.registerScalarFunction(
            db, 'wasm_registry_version', 0,
            { deterministic: true },
            FUNC_WASM_REGISTRY_VERSION
        ));
        assert(true, 'wasm_registry_version() registered');

        // wasm_sync()
        handles.push(extension.registerScalarFunction(
            db, 'wasm_sync', 0,
            { deterministic: false },
            FUNC_WASM_SYNC
        ));
        assert(true, 'wasm_sync() registered');

        // wasm_search(query)
        handles.push(extension.registerScalarFunction(
            db, 'wasm_search', 1,
            { deterministic: false },
            FUNC_WASM_SEARCH
        ));
        assert(true, 'wasm_search() registered');

        // wasm_list()
        handles.push(extension.registerScalarFunction(
            db, 'wasm_list', 0,
            { deterministic: false },
            FUNC_WASM_LIST
        ));
        assert(true, 'wasm_list() registered');

        // wasm_install(name)
        handles.push(extension.registerScalarFunction(
            db, 'wasm_install', 1,
            { deterministic: false },
            FUNC_WASM_INSTALL
        ));
        assert(true, 'wasm_install(name) registered');

        // wasm_install(name, version)
        handles.push(extension.registerScalarFunction(
            db, 'wasm_install', 2,
            { deterministic: false },
            FUNC_WASM_INSTALL
        ));
        assert(true, 'wasm_install(name, version) registered');

        // wasm_uninstall(name)
        handles.push(extension.registerScalarFunction(
            db, 'wasm_uninstall', 1,
            { deterministic: false },
            FUNC_WASM_UNINSTALL
        ));
        assert(true, 'wasm_uninstall() registered');

        // wasm_info(name)
        handles.push(extension.registerScalarFunction(
            db, 'wasm_info', 1,
            { deterministic: false },
            FUNC_WASM_INFO
        ));
        assert(true, 'wasm_info() registered');

        // wasm_update()
        handles.push(extension.registerScalarFunction(
            db, 'wasm_update', 0,
            { deterministic: false },
            FUNC_WASM_UPDATE
        ));
        assert(true, 'wasm_update() registered');

        // wasm_update(name)
        handles.push(extension.registerScalarFunction(
            db, 'wasm_update', 1,
            { deterministic: false },
            FUNC_WASM_UPDATE
        ));
        assert(true, 'wasm_update(name) registered');

        // wasm_init()
        handles.push(extension.registerScalarFunction(
            db, 'wasm_init', 0,
            { deterministic: true },
            FUNC_WASM_INIT
        ));
        assert(true, 'wasm_init() registered');

    } catch (e) {
        failed++;
        console.log(`  ✗ Function registration failed: ${e.message}`);
    }

    // Test wasm_registry_version()
    console.log('\n3. Testing wasm_registry_version()\n');
    try {
        const stmt = lowLevel.prepare(db, 'SELECT wasm_registry_version()');
        lowLevel.step(stmt);
        const version = lowLevel.columnText(stmt, 0);
        lowLevel.finalize(stmt);
        assertEqual(version, '1.0.0', 'wasm_registry_version() returns correct version');
    } catch (e) {
        failed++;
        console.log(`  ✗ wasm_registry_version() test failed: ${e.message}`);
    }

    // Test wasm_sync()
    console.log('\n4. Testing wasm_sync()\n');
    try {
        const stmt = lowLevel.prepare(db, 'SELECT wasm_sync()');
        lowLevel.step(stmt);
        const result = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(result.success === true, 'wasm_sync() returns success');
        assert(result.extensions_count > 0, `wasm_sync() reports ${result.extensions_count} extensions`);
        assert(result.last_sync !== null, 'wasm_sync() reports last sync time');
    } catch (e) {
        failed++;
        console.log(`  ✗ wasm_sync() test failed: ${e.message}`);
    }

    // Test wasm_search()
    console.log('\n5. Testing wasm_search()\n');
    try {
        // Search for "text"
        let stmt = lowLevel.prepare(db, "SELECT wasm_search('text')");
        lowLevel.step(stmt);
        let results = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(Array.isArray(results), 'wasm_search() returns array');
        assert(results.length > 0, `wasm_search('text') found ${results.length} extensions`);
        assert(results.some(e => e.name === 'text'), 'wasm_search() found text extension');

        // Search for "hash" (crypto extension keyword)
        stmt = lowLevel.prepare(db, "SELECT wasm_search('hash')");
        lowLevel.step(stmt);
        results = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(results.some(e => e.name === 'crypto'), 'wasm_search() found crypto extension via keyword');

        // Search for non-existent
        stmt = lowLevel.prepare(db, "SELECT wasm_search('nonexistent12345')");
        lowLevel.step(stmt);
        results = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assertEqual(results.length, 0, 'wasm_search() returns empty array for no matches');

    } catch (e) {
        failed++;
        console.log(`  ✗ wasm_search() test failed: ${e.message}`);
    }

    // Test wasm_info()
    console.log('\n6. Testing wasm_info()\n');
    try {
        const stmt = lowLevel.prepare(db, "SELECT wasm_info('text')");
        lowLevel.step(stmt);
        const info = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assertEqual(info.name, 'text', 'wasm_info() returns correct name');
        assertEqual(info.version, '0.1.0', 'wasm_info() returns correct version');
        assert(info.exports.length > 0, `wasm_info() shows ${info.exports.length} exports`);
        assertEqual(info.installed, false, 'wasm_info() shows not installed');

        // Test non-existent extension
        const stmt2 = lowLevel.prepare(db, "SELECT wasm_info('nonexistent')");
        lowLevel.step(stmt2);
        const info2 = JSON.parse(lowLevel.columnText(stmt2, 0));
        lowLevel.finalize(stmt2);

        assert(info2.error !== undefined, 'wasm_info() returns error for non-existent extension');

    } catch (e) {
        failed++;
        console.log(`  ✗ wasm_info() test failed: ${e.message}`);
    }

    // Test wasm_install()
    console.log('\n7. Testing wasm_install()\n');
    try {
        // Try to install text extension
        let stmt = lowLevel.prepare(db, "SELECT wasm_install('text')");
        lowLevel.step(stmt);
        let result = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(result.success === true, 'wasm_install() returns success');
        assertEqual(result.action, 'install', 'wasm_install() action is install');
        assertEqual(result.name, 'text', 'wasm_install() has correct name');
        assert(result.message.includes('queued'), 'wasm_install() indicates queued');

        // Mark as installed (simulating host completion)
        markExtensionInstalled('text', '0.1.0');

        // Try to install again (should fail)
        stmt = lowLevel.prepare(db, "SELECT wasm_install('text')");
        lowLevel.step(stmt);
        result = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(result.success === false, 'wasm_install() fails for already installed');
        assertContains(result.error, 'already installed', 'wasm_install() error mentions already installed');

        // Try to install non-existent
        stmt = lowLevel.prepare(db, "SELECT wasm_install('nonexistent')");
        lowLevel.step(stmt);
        result = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(result.success === false, 'wasm_install() fails for non-existent');
        assertContains(result.error, 'not found', 'wasm_install() error mentions not found');

    } catch (e) {
        failed++;
        console.log(`  ✗ wasm_install() test failed: ${e.message}`);
    }

    // Test wasm_list()
    console.log('\n8. Testing wasm_list()\n');
    try {
        const stmt = lowLevel.prepare(db, 'SELECT wasm_list()');
        lowLevel.step(stmt);
        const list = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(Array.isArray(list), 'wasm_list() returns array');
        assertEqual(list.length, 1, 'wasm_list() shows 1 installed extension');
        assertEqual(list[0].name, 'text', 'wasm_list() shows text extension');
        assertEqual(list[0].version, '0.1.0', 'wasm_list() shows correct version');
        assert(list[0].installed_at !== undefined, 'wasm_list() shows installed_at');

    } catch (e) {
        failed++;
        console.log(`  ✗ wasm_list() test failed: ${e.message}`);
    }

    // Test wasm_update()
    console.log('\n9. Testing wasm_update()\n');
    try {
        // With matching version, should be up to date
        let stmt = lowLevel.prepare(db, "SELECT wasm_update('text')");
        lowLevel.step(stmt);
        let result = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(result.success === true, 'wasm_update() returns success');
        assertContains(result.message, 'latest version', 'wasm_update() indicates already latest');

        // Simulate old version
        markExtensionUninstalled('text');
        markExtensionInstalled('text', '0.0.9');

        // Now update should find an update
        stmt = lowLevel.prepare(db, "SELECT wasm_update('text')");
        lowLevel.step(stmt);
        result = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(result.updates.length === 1, 'wasm_update() found 1 update');
        assertEqual(result.updates[0].old_version, '0.0.9', 'wasm_update() shows old version');
        assertEqual(result.updates[0].new_version, '0.1.0', 'wasm_update() shows new version');

        // Test update for non-installed
        stmt = lowLevel.prepare(db, "SELECT wasm_update('uuid')");
        lowLevel.step(stmt);
        result = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(result.success === false, 'wasm_update() fails for non-installed');

    } catch (e) {
        failed++;
        console.log(`  ✗ wasm_update() test failed: ${e.message}`);
    }

    // Test wasm_uninstall()
    console.log('\n10. Testing wasm_uninstall()\n');
    try {
        // Uninstall text extension
        let stmt = lowLevel.prepare(db, "SELECT wasm_uninstall('text')");
        lowLevel.step(stmt);
        let result = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(result.success === true, 'wasm_uninstall() returns success');
        assertEqual(result.action, 'uninstall', 'wasm_uninstall() action is uninstall');
        assertEqual(result.name, 'text', 'wasm_uninstall() has correct name');

        // Mark as uninstalled
        markExtensionUninstalled('text');

        // Try to uninstall again (should fail)
        stmt = lowLevel.prepare(db, "SELECT wasm_uninstall('text')");
        lowLevel.step(stmt);
        result = JSON.parse(lowLevel.columnText(stmt, 0));
        lowLevel.finalize(stmt);

        assert(result.success === false, 'wasm_uninstall() fails for non-installed');
        assertContains(result.error, 'is not installed', 'wasm_uninstall() error mentions not installed');

    } catch (e) {
        failed++;
        console.log(`  ✗ wasm_uninstall() test failed: ${e.message}`);
    }

    // Test wasm_init()
    console.log('\n11. Testing wasm_init()\n');
    try {
        const stmt = lowLevel.prepare(db, 'SELECT wasm_init()');
        lowLevel.step(stmt);
        const sql = lowLevel.columnText(stmt, 0);
        lowLevel.finalize(stmt);

        assertContains(sql, 'CREATE TABLE IF NOT EXISTS _wasm_extensions', 'wasm_init() creates extensions table');
        assertContains(sql, 'CREATE TABLE IF NOT EXISTS _wasm_installed', 'wasm_init() creates installed table');
        assertContains(sql, 'CREATE TABLE IF NOT EXISTS _wasm_registry_meta', 'wasm_init() creates meta table');

        // Execute the SQL to verify it's valid
        lowLevel.exec(db, sql);
        assert(true, 'wasm_init() SQL executes successfully');

    } catch (e) {
        failed++;
        console.log(`  ✗ wasm_init() test failed: ${e.message}`);
    }

    // Test getExtensionManagerState()
    console.log('\n12. Testing State Persistence\n');
    try {
        // Install some extensions
        markExtensionInstalled('text', '0.1.0');
        markExtensionInstalled('uuid', '0.1.0');

        const state = getExtensionManagerState();

        assert(state.registry !== null, 'State includes registry');
        assert(Object.keys(state.installed).length === 2, 'State includes 2 installed extensions');
        assertEqual(state.installed.text.version, '0.1.0', 'State has correct text version');
        assertEqual(state.installed.uuid.version, '0.1.0', 'State has correct uuid version');

    } catch (e) {
        failed++;
        console.log(`  ✗ State persistence test failed: ${e.message}`);
    }

    // Close database
    lowLevel.close(db);

    // Summary
    console.log('\n' + '='.repeat(50));
    console.log(`Tests: ${passed + failed} total, ${passed} passed, ${failed} failed`);
    console.log('='.repeat(50) + '\n');

    if (failed > 0) {
        process.exit(1);
    }
}

runTests().catch(err => {
    console.error('Test error:', err);
    process.exit(1);
});
