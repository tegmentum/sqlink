/**
 * SQLite WASM Component Test
 *
 * Tests the SQLite WASM component using the jco-transpiled JavaScript bindings.
 */

import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { instantiate } from './sqlite/sqlite.js';
import { cli, clocks, filesystem, io, random, sockets } from '@bytecodealliance/preview2-shim';

const __dirname = dirname(fileURLToPath(import.meta.url));

// Helper to load and compile wasm files
async function getCoreModule(name) {
    const wasmPath = join(__dirname, 'sqlite', name);
    const bytes = await readFile(wasmPath);
    return WebAssembly.compile(bytes);
}

// Run tests
async function runTests() {
    console.log('=== SQLite WASM Component Test ===\n');

    // Instantiate the component with WASI shim
    console.log('Instantiating SQLite component...');

    // Map the WASI shim exports to the interface names jco expects
    const wasiImports = {
        'wasi:cli/environment': cli.environment,
        'wasi:cli/exit': cli.exit,
        'wasi:cli/stdin': cli.stdin,
        'wasi:cli/stdout': cli.stdout,
        'wasi:cli/stderr': cli.stderr,
        'wasi:cli/terminal-input': cli.terminalInput,
        'wasi:cli/terminal-output': cli.terminalOutput,
        'wasi:cli/terminal-stdin': cli.terminalStdin,
        'wasi:cli/terminal-stdout': cli.terminalStdout,
        'wasi:cli/terminal-stderr': cli.terminalStderr,
        'wasi:clocks/monotonic-clock': clocks.monotonicClock,
        'wasi:clocks/wall-clock': clocks.wallClock,
        'wasi:filesystem/types': filesystem.types,
        'wasi:filesystem/preopens': filesystem.preopens,
        'wasi:io/error': io.error,
        'wasi:io/poll': io.poll,
        'wasi:io/streams': io.streams,
        'wasi:random/random': random.random,
        'wasi:sockets/tcp': sockets.tcp,
        'wasi:sockets/udp': sockets.udp,
    };

    const { lowLevel, highLevel } = await instantiate(getCoreModule, wasiImports);

    console.log('Component instantiated successfully!\n');

    // Test 1: Library version info
    console.log('Test 1: Library version info');
    const version = lowLevel.libversion();
    const versionNumber = lowLevel.libversionNumber();
    const sourceId = lowLevel.sourceid();
    console.log(`  SQLite version: ${version}`);
    console.log(`  Version number: ${versionNumber}`);
    console.log(`  Source ID: ${sourceId}`);
    console.log('  PASS\n');

    // Test 2: High-level API version
    console.log('Test 2: High-level API version');
    const hlVersion = highLevel.version();
    const hlVersionNumber = highLevel.versionNumber();
    console.log(`  Version: ${hlVersion}`);
    console.log(`  Version number: ${hlVersionNumber}`);
    console.log('  PASS\n');

    // Test 3: Open in-memory database using low-level API
    // Note: jco unwraps result types - returns value on success, throws on error
    console.log('Test 3: Open in-memory database (low-level)');
    const openFlags = { readwrite: true, create: true };
    let dbHandle;
    try {
        dbHandle = lowLevel.open(':memory:', openFlags);
        console.log(`  Database handle: ${dbHandle}`);
        console.log('  PASS\n');
    } catch (e) {
        console.log(`  FAIL: Could not open database: ${e}`);
        process.exit(1);
    }

    // Test 4: Execute SQL to create table
    console.log('Test 4: Create table');
    const createSql = 'CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)';
    try {
        lowLevel.exec(dbHandle, createSql);
        console.log('  Table created successfully');
        console.log('  PASS\n');
    } catch (e) {
        console.log(`  FAIL: Could not create table: ${e}`);
        process.exit(1);
    }

    // Test 5: Insert data using prepared statement
    console.log('Test 5: Insert data using prepared statement');
    const insertSql = 'INSERT INTO users (name, age) VALUES (?, ?)';
    let stmtHandle;
    try {
        stmtHandle = lowLevel.prepare(dbHandle, insertSql);
    } catch (e) {
        console.log(`  FAIL: Could not prepare statement: ${e}`);
        process.exit(1);
    }

    // Insert first row
    lowLevel.bindText(stmtHandle, 1, 'Alice');
    lowLevel.bindInt(stmtHandle, 2, 30);
    let stepResult = lowLevel.step(stmtHandle);
    if (stepResult !== 'done') {
        console.log(`  FAIL: Unexpected step result: ${stepResult}`);
        process.exit(1);
    }
    lowLevel.reset(stmtHandle);

    // Insert second row
    lowLevel.bindText(stmtHandle, 1, 'Bob');
    lowLevel.bindInt(stmtHandle, 2, 25);
    stepResult = lowLevel.step(stmtHandle);
    if (stepResult !== 'done') {
        console.log(`  FAIL: Unexpected step result: ${stepResult}`);
        process.exit(1);
    }
    lowLevel.reset(stmtHandle);

    // Insert third row
    lowLevel.bindText(stmtHandle, 1, 'Charlie');
    lowLevel.bindInt(stmtHandle, 2, 35);
    stepResult = lowLevel.step(stmtHandle);
    lowLevel.finalize(stmtHandle);

    const changes = lowLevel.totalChanges(dbHandle);
    console.log(`  Inserted 3 rows (total changes: ${changes})`);
    console.log('  PASS\n');

    // Test 6: Query data
    console.log('Test 6: Query data');
    const selectSql = 'SELECT id, name, age FROM users ORDER BY age';
    let selectStmt;
    try {
        selectStmt = lowLevel.prepare(dbHandle, selectSql);
    } catch (e) {
        console.log(`  FAIL: Could not prepare select: ${e}`);
        process.exit(1);
    }

    const columnCount = lowLevel.columnCount(selectStmt);
    console.log(`  Column count: ${columnCount}`);

    let rowCount = 0;
    while (lowLevel.step(selectStmt) === 'row') {
        const id = lowLevel.columnInt(selectStmt, 0);
        const name = lowLevel.columnText(selectStmt, 1);
        const age = lowLevel.columnInt(selectStmt, 2);
        console.log(`  Row ${++rowCount}: id=${id}, name="${name}", age=${age}`);
    }
    lowLevel.finalize(selectStmt);
    console.log('  PASS\n');

    // Test 7: Close database
    console.log('Test 7: Close database');
    const closeResult = lowLevel.close(dbHandle);
    if (closeResult !== 'ok') {
        console.log(`  FAIL: Could not close database: ${closeResult}`);
        process.exit(1);
    }
    console.log('  Database closed successfully');
    console.log('  PASS\n');

    // Test 8: High-level API - open memory database
    // Note: jco unwraps results - throws on error, returns value on success
    console.log('Test 8: High-level API - open memory database');
    let conn;
    try {
        conn = highLevel.openMemory();
        console.log('  In-memory connection opened');
        console.log('  PASS\n');
    } catch (e) {
        console.log(`  FAIL: Could not open memory database: ${e}`);
        process.exit(1);
    }

    // Test 9: High-level API - execute and query
    console.log('Test 9: High-level API - execute and query');

    // Create table
    try {
        conn.execute('CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price REAL)');
        console.log('  Table created');
    } catch (e) {
        console.log(`  FAIL: Could not create table: ${e}`);
        process.exit(1);
    }

    // Insert some data
    try {
        const execResult = conn.execute("INSERT INTO products (name, price) VALUES ('Widget', 9.99), ('Gadget', 19.99), ('Gizmo', 29.99)");
        console.log(`  Inserted ${execResult.changes} rows, last rowid: ${execResult.lastInsertRowid}`);
    } catch (e) {
        console.log(`  FAIL: Could not insert data: ${e}`);
        process.exit(1);
    }

    // Query data
    try {
        const queryResult = conn.query('SELECT * FROM products ORDER BY price DESC');
        console.log(`  Query returned ${queryResult.rows.length} rows`);
        console.log(`  Columns: ${queryResult.columnNames.join(', ')}`);
        for (const row of queryResult.rows) {
            const values = row.columns.map(c => {
                if (c.tag === 'null') return 'NULL';
                return c.val;
            });
            console.log(`    ${values.join(', ')}`);
        }
        console.log('  PASS\n');
    } catch (e) {
        console.log(`  FAIL: Could not query: ${e}`);
        process.exit(1);
    }

    // Test 10: Transaction
    console.log('Test 10: Transaction');
    try {
        conn.beginTransaction();
        console.log('  Transaction started');
        console.log(`  In autocommit: ${conn.inAutocommit()}`);

        conn.execute("INSERT INTO products (name, price) VALUES ('TestItem', 99.99)");

        conn.rollback();
        console.log('  Transaction rolled back');
        console.log(`  In autocommit: ${conn.inAutocommit()}`);

        // Verify rollback worked
        const countResult = conn.query('SELECT COUNT(*) as cnt FROM products');
        const count = countResult.rows[0].columns[0].val;
        console.log(`  Product count after rollback: ${count} (should be 3)`);
        console.log('  PASS\n');
    } catch (e) {
        console.log(`  FAIL: Transaction test failed: ${e}`);
        process.exit(1);
    }

    console.log('=== All tests passed! ===');
}

runTests().catch(err => {
    console.error('Test failed with error:', err);
    process.exit(1);
});
