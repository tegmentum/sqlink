/**
 * SQLite WASM Extension API Tests
 * Tests custom scalar functions, aggregate functions, collations, and hooks
 */

import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

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

function assertMatch(actual, pattern, message) {
    if (pattern.test(actual)) {
        passed++;
        console.log(`  ✓ ${message}`);
    } else {
        failed++;
        console.log(`  ✗ ${message}`);
        console.log(`    Pattern: ${pattern}`);
        console.log(`    Actual: ${actual}`);
    }
}

async function runTests() {
    console.log('SQLite WASM Extension API Tests\n');

    // Import the extension callbacks constants
    const {
        FUNC_UUID,
        FUNC_REGEXP,
        FUNC_REVERSE,
        FUNC_MATH_SQRT,
        FUNC_GROUP_CONCAT,
        COLLATION_NOCASE_REVERSE,
        registerUpdateHook,
        registerCommitHook
    } = await import(join(__dirname, '../../../build/js-ext/extension-callbacks.js'));

    // Import the extensible SQLite component
    const sqlite = await import(join(__dirname, '../../../build/js-ext/sqlite-extensible.js'));
    const { lowLevel, extension } = sqlite;

    console.log('1. Testing scalar functions\n');

    // Open database
    const openFlags = { readwrite: true, create: true, memory: true };
    const db = lowLevel.open(':memory:', openFlags);
    assert(db !== 0n, 'Database opened successfully');

    // Register uuid() function
    console.log('\n  Registering uuid() function...');
    try {
        const uuidHandle = extension.registerScalarFunction(
            db,
            'uuid',
            0, // no args
            { deterministic: false },
            FUNC_UUID
        );
        assert(true, 'uuid() function registered');

        // Test uuid()
        const stmt = lowLevel.prepare(db, 'SELECT uuid()');
        const stepResult = lowLevel.step(stmt);
        assertEqual(stepResult, 'row', 'uuid() returned a row');

        const uuidValue = lowLevel.columnText(stmt, 0);
        assertMatch(uuidValue, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
            `uuid() returned valid UUID v4: ${uuidValue}`);

        lowLevel.finalize(stmt);

        // Test uuid uniqueness
        const stmt2 = lowLevel.prepare(db, 'SELECT uuid(), uuid()');
        lowLevel.step(stmt2);
        const uuid1 = lowLevel.columnText(stmt2, 0);
        const uuid2 = lowLevel.columnText(stmt2, 1);
        assert(uuid1 !== uuid2, 'uuid() generates unique values');
        lowLevel.finalize(stmt2);

    } catch (e) {
        failed++;
        console.log(`  ✗ uuid() test failed: ${e.message}`);
    }

    // Register regexp() function
    console.log('\n  Registering regexp() function...');
    try {
        const regexpHandle = extension.registerScalarFunction(
            db,
            'regexp',
            2, // 2 args: pattern, text
            { deterministic: true },
            FUNC_REGEXP
        );
        assert(true, 'regexp() function registered');

        // Test regexp() - match
        let stmt = lowLevel.prepare(db, "SELECT regexp('^hello', 'hello world')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 1, "regexp('^hello', 'hello world') = 1");
        lowLevel.finalize(stmt);

        // Test regexp() - no match
        stmt = lowLevel.prepare(db, "SELECT regexp('^world', 'hello world')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 0, "regexp('^world', 'hello world') = 0");
        lowLevel.finalize(stmt);

        // Test regexp() with email pattern
        stmt = lowLevel.prepare(db, "SELECT regexp('[a-z]+@[a-z]+\\.[a-z]+', 'test@example.com')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 1, 'regexp() matches email pattern');
        lowLevel.finalize(stmt);

    } catch (e) {
        failed++;
        console.log(`  ✗ regexp() test failed: ${e.message}`);
    }

    // Register reverse() function
    console.log('\n  Registering reverse() function...');
    try {
        const reverseHandle = extension.registerScalarFunction(
            db,
            'reverse',
            1,
            { deterministic: true },
            FUNC_REVERSE
        );
        assert(true, 'reverse() function registered');

        // Test reverse()
        let stmt = lowLevel.prepare(db, "SELECT reverse('hello')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'olleh', "reverse('hello') = 'olleh'");
        lowLevel.finalize(stmt);

        // Test reverse() with unicode
        stmt = lowLevel.prepare(db, "SELECT reverse('abc123')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), '321cba', "reverse('abc123') = '321cba'");
        lowLevel.finalize(stmt);

    } catch (e) {
        failed++;
        console.log(`  ✗ reverse() test failed: ${e.message}`);
    }

    // Register math_sqrt() function
    console.log('\n  Registering math_sqrt() function...');
    try {
        const sqrtHandle = extension.registerScalarFunction(
            db,
            'math_sqrt',
            1,
            { deterministic: true },
            FUNC_MATH_SQRT
        );
        assert(true, 'math_sqrt() function registered');

        // Test math_sqrt()
        let stmt = lowLevel.prepare(db, "SELECT math_sqrt(16)");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnDouble(stmt, 0), 4.0, 'math_sqrt(16) = 4.0');
        lowLevel.finalize(stmt);

        // Test math_sqrt() with decimal
        stmt = lowLevel.prepare(db, "SELECT math_sqrt(2)");
        lowLevel.step(stmt);
        const sqrt2 = lowLevel.columnDouble(stmt, 0);
        assert(Math.abs(sqrt2 - Math.SQRT2) < 0.0000001, `math_sqrt(2) ≈ ${Math.SQRT2}`);
        lowLevel.finalize(stmt);

    } catch (e) {
        failed++;
        console.log(`  ✗ math_sqrt() test failed: ${e.message}`);
    }

    console.log('\n2. Testing aggregate functions\n');

    // Register group_concat_custom() function
    console.log('  Registering group_concat_custom() function...');
    try {
        const gcHandle = extension.registerAggregateFunction(
            db,
            'group_concat_custom',
            2, // value, separator
            { deterministic: true },
            FUNC_GROUP_CONCAT
        );
        assert(true, 'group_concat_custom() function registered');

        // Create test table
        lowLevel.exec(db, 'CREATE TABLE items (name TEXT)');
        lowLevel.exec(db, "INSERT INTO items VALUES ('apple'), ('banana'), ('cherry')");

        // Test group_concat_custom()
        let stmt = lowLevel.prepare(db, "SELECT group_concat_custom(name, ' | ') FROM items");
        lowLevel.step(stmt);
        const result = lowLevel.columnText(stmt, 0);
        assertEqual(result, 'apple | banana | cherry', 'group_concat_custom with custom separator');
        lowLevel.finalize(stmt);

    } catch (e) {
        failed++;
        console.log(`  ✗ group_concat_custom() test failed: ${e.message}`);
    }

    console.log('\n3. Testing collations\n');

    // Register custom collation
    console.log('  Registering nocase_reverse collation...');
    try {
        const collHandle = extension.registerCollation(
            db,
            'nocase_reverse',
            COLLATION_NOCASE_REVERSE
        );
        assert(true, 'nocase_reverse collation registered');

        // Create test table with collation
        lowLevel.exec(db, 'CREATE TABLE words (word TEXT COLLATE nocase_reverse)');
        lowLevel.exec(db, "INSERT INTO words VALUES ('Apple'), ('Banana'), ('cherry')");

        // Test ordering with custom collation
        let stmt = lowLevel.prepare(db, 'SELECT word FROM words ORDER BY word');
        const words = [];
        while (lowLevel.step(stmt) === 'row') {
            words.push(lowLevel.columnText(stmt, 0));
        }
        lowLevel.finalize(stmt);

        // With reverse ordering, should be: cherry, Banana, Apple (reverse alphabetical, case-insensitive)
        assertEqual(words.join(', '), 'cherry, Banana, Apple', 'nocase_reverse collation orders correctly');

    } catch (e) {
        failed++;
        console.log(`  ✗ collation test failed: ${e.message}`);
    }

    console.log('\n4. Testing hooks\n');

    // Test update hook
    console.log('  Testing update hook...');
    try {
        const updates = [];
        registerUpdateHook(100n, (op, database, table, rowid) => {
            updates.push({ op, database, table, rowid: Number(rowid) });
        });

        const hookHandle = extension.setUpdateHook(db, 100n);
        assert(true, 'Update hook set');

        // Perform operations
        lowLevel.exec(db, 'CREATE TABLE hook_test (id INTEGER PRIMARY KEY, value TEXT)');
        lowLevel.exec(db, "INSERT INTO hook_test VALUES (1, 'test')");
        lowLevel.exec(db, "UPDATE hook_test SET value = 'updated' WHERE id = 1");
        lowLevel.exec(db, "DELETE FROM hook_test WHERE id = 1");

        assertEqual(updates.length, 3, 'Update hook called 3 times');
        if (updates.length >= 3) {
            assertEqual(updates[0].op, 'insert', 'First operation was insert');
            assertEqual(updates[1].op, 'update', 'Second operation was update');
            assertEqual(updates[2].op, 'delete', 'Third operation was delete');
        }

        extension.removeUpdateHook(hookHandle);
        assert(true, 'Update hook removed');

    } catch (e) {
        failed++;
        console.log(`  ✗ update hook test failed: ${e.message}`);
    }

    // Test commit hook
    console.log('\n  Testing commit hook...');
    try {
        let commitCount = 0;
        registerCommitHook(101n, () => {
            commitCount++;
            return false; // Allow commit
        });

        const commitHandle = extension.setCommitHook(db, 101n);
        assert(true, 'Commit hook set');

        // Perform transaction
        lowLevel.exec(db, 'BEGIN');
        lowLevel.exec(db, 'CREATE TABLE commit_test (x INTEGER)');
        lowLevel.exec(db, 'INSERT INTO commit_test VALUES (1)');
        lowLevel.exec(db, 'COMMIT');

        assertEqual(commitCount, 1, 'Commit hook called once');

        extension.removeCommitHook(commitHandle);

    } catch (e) {
        failed++;
        console.log(`  ✗ commit hook test failed: ${e.message}`);
    }

    console.log('\n5. Testing functions in complex queries\n');

    try {
        // Use multiple custom functions together
        let stmt = lowLevel.prepare(db, `
            SELECT
                reverse(uuid()) as reversed_uuid,
                math_sqrt(length('hello')) as sqrt_len
        `);
        lowLevel.step(stmt);

        const reversedUuid = lowLevel.columnText(stmt, 0);
        const sqrtLen = lowLevel.columnDouble(stmt, 1);

        assertEqual(reversedUuid.length, 36, 'reversed uuid has correct length');
        assert(Math.abs(sqrtLen - Math.sqrt(5)) < 0.0001, 'sqrt(length("hello")) = sqrt(5)');

        lowLevel.finalize(stmt);

        // Test in WHERE clause
        lowLevel.exec(db, 'CREATE TABLE emails (email TEXT)');
        lowLevel.exec(db, "INSERT INTO emails VALUES ('test@example.com'), ('invalid'), ('user@domain.org')");

        stmt = lowLevel.prepare(db, "SELECT email FROM emails WHERE regexp('[a-z]+@[a-z]+\\.[a-z]+', email)");
        const validEmails = [];
        while (lowLevel.step(stmt) === 'row') {
            validEmails.push(lowLevel.columnText(stmt, 0));
        }
        lowLevel.finalize(stmt);

        assertEqual(validEmails.length, 2, 'regexp found 2 valid emails');
        assertEqual(validEmails.join(', '), 'test@example.com, user@domain.org', 'Correct emails found');

    } catch (e) {
        failed++;
        console.log(`  ✗ complex query test failed: ${e.message}`);
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
