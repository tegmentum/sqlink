/**
 * SQLite WASM Text Extension Test
 * Tests the sqlean text extension functions
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

async function runTests() {
    console.log('SQLite WASM Text Extension Tests\n');

    // Import the merged extension callbacks from build/js-ext
    const callbacks = await import(join(__dirname, '../../../build/js-ext/extension-callbacks.js'));

    // Import the extensible SQLite component
    const sqlite = await import(join(__dirname, '../../../build/js-ext/sqlite-extensible.js'));
    const { lowLevel, extension } = sqlite;

    console.log('1. Testing text functions\n');

    // Open database
    const openFlags = { readwrite: true, create: true, memory: true };
    const db = lowLevel.open(':memory:', openFlags);
    assert(db !== 0n, 'Database opened successfully');

    // Register text extension functions manually
    console.log('\n  Registering text extension functions...');
    try {
        const funcFlags = { deterministic: true };

        // Register the text functions we're testing
        extension.registerScalarFunction(db, 'reverse', 1, funcFlags, callbacks.FUNC_TEXT_REVERSE);
        extension.registerScalarFunction(db, 'left', 2, funcFlags, callbacks.FUNC_LEFT);
        extension.registerScalarFunction(db, 'right', 2, funcFlags, callbacks.FUNC_RIGHT);
        extension.registerScalarFunction(db, 'strpos', 2, funcFlags, callbacks.FUNC_STRPOS);
        extension.registerScalarFunction(db, 'text_contains', 2, funcFlags, callbacks.FUNC_TEXT_CONTAINS);
        extension.registerScalarFunction(db, 'starts_with', 2, funcFlags, callbacks.FUNC_STARTS_WITH);
        extension.registerScalarFunction(db, 'text_has_suffix', 2, funcFlags, callbacks.FUNC_TEXT_HAS_SUFFIX);
        extension.registerScalarFunction(db, 'repeat', 2, funcFlags, callbacks.FUNC_REPEAT);
        extension.registerScalarFunction(db, 'lpad', -1, funcFlags, callbacks.FUNC_LPAD);
        extension.registerScalarFunction(db, 'rpad', -1, funcFlags, callbacks.FUNC_RPAD);
        extension.registerScalarFunction(db, 'split_part', 3, funcFlags, callbacks.FUNC_SPLIT_PART);
        extension.registerScalarFunction(db, 'concat_ws', -1, funcFlags, callbacks.FUNC_CONCAT_WS);
        extension.registerScalarFunction(db, 'text_upper', 1, funcFlags, callbacks.FUNC_TEXT_UPPER);
        extension.registerScalarFunction(db, 'text_lower', 1, funcFlags, callbacks.FUNC_TEXT_LOWER);
        extension.registerScalarFunction(db, 'text_replace', 3, funcFlags, callbacks.FUNC_TEXT_REPLACE);
        extension.registerScalarFunction(db, 'char_length', 1, funcFlags, callbacks.FUNC_CHAR_LENGTH);
        extension.registerScalarFunction(db, 'text_count', 2, funcFlags, callbacks.FUNC_TEXT_COUNT);
        extension.registerScalarFunction(db, 'sqlean_version', 0, { deterministic: false }, callbacks.FUNC_SQLEAN_VERSION);

        assert(true, 'Text extension functions registered');
    } catch (e) {
        failed++;
        console.log(`  ✗ Failed to register text functions: ${e.message}`);
        console.log(e.stack);
        process.exit(1);
    }

    // Test reverse
    console.log('\n2. Testing reverse()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT reverse('hello')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'olleh', "reverse('hello') = 'olleh'");
        lowLevel.finalize(stmt);

        stmt = lowLevel.prepare(db, "SELECT reverse('abcd1234')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), '4321dcba', "reverse('abcd1234') = '4321dcba'");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ reverse() test failed: ${e.message}`);
    }

    // Test left/right
    console.log('\n3. Testing left() and right()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT left('hello world', 5)");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'hello', "left('hello world', 5) = 'hello'");
        lowLevel.finalize(stmt);

        stmt = lowLevel.prepare(db, "SELECT right('hello world', 5)");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'world', "right('hello world', 5) = 'world'");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ left/right test failed: ${e.message}`);
    }

    // Test strpos
    console.log('\n4. Testing strpos()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT strpos('hello world', 'world')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 7, "strpos('hello world', 'world') = 7");
        lowLevel.finalize(stmt);

        stmt = lowLevel.prepare(db, "SELECT strpos('hello world', 'xyz')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 0, "strpos('hello world', 'xyz') = 0");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ strpos test failed: ${e.message}`);
    }

    // Test text_contains
    console.log('\n5. Testing text_contains()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT text_contains('hello world', 'wor')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 1, "text_contains('hello world', 'wor') = 1");
        lowLevel.finalize(stmt);

        stmt = lowLevel.prepare(db, "SELECT text_contains('hello world', 'xyz')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 0, "text_contains('hello world', 'xyz') = 0");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ text_contains test failed: ${e.message}`);
    }

    // Test starts_with and text_has_suffix
    console.log('\n6. Testing starts_with() and text_has_suffix()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT starts_with('hello world', 'hello')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 1, "starts_with('hello world', 'hello') = 1");
        lowLevel.finalize(stmt);

        stmt = lowLevel.prepare(db, "SELECT text_has_suffix('hello world', 'world')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 1, "text_has_suffix('hello world', 'world') = 1");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ starts_with/text_has_suffix test failed: ${e.message}`);
    }

    // Test repeat
    console.log('\n7. Testing repeat()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT repeat('ab', 3)");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'ababab', "repeat('ab', 3) = 'ababab'");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ repeat test failed: ${e.message}`);
    }

    // Test lpad/rpad
    console.log('\n8. Testing lpad() and rpad()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT lpad('test', 8, '*')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), '****test', "lpad('test', 8, '*') = '****test'");
        lowLevel.finalize(stmt);

        stmt = lowLevel.prepare(db, "SELECT rpad('test', 8, '*')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'test****', "rpad('test', 8, '*') = 'test****'");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ lpad/rpad test failed: ${e.message}`);
    }

    // Test split_part
    console.log('\n9. Testing split_part()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT split_part('a,b,c,d', ',', 2)");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'b', "split_part('a,b,c,d', ',', 2) = 'b'");
        lowLevel.finalize(stmt);

        stmt = lowLevel.prepare(db, "SELECT split_part('hello world test', ' ', 3)");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'test', "split_part('hello world test', ' ', 3) = 'test'");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ split_part test failed: ${e.message}`);
    }

    // Test concat_ws
    console.log('\n10. Testing concat_ws()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT concat_ws(', ', 'apple', 'banana', 'cherry')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'apple, banana, cherry', "concat_ws(', ', 'apple', 'banana', 'cherry')");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ concat_ws test failed: ${e.message}`);
    }

    // Test text_upper/text_lower
    console.log('\n11. Testing text_upper() and text_lower()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT text_upper('Hello World')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'HELLO WORLD', "text_upper('Hello World') = 'HELLO WORLD'");
        lowLevel.finalize(stmt);

        stmt = lowLevel.prepare(db, "SELECT text_lower('Hello World')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'hello world', "text_lower('Hello World') = 'hello world'");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ text_upper/text_lower test failed: ${e.message}`);
    }

    // Test text_replace
    console.log('\n12. Testing text_replace()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT text_replace('hello world', 'world', 'universe')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnText(stmt, 0), 'hello universe', "text_replace('hello world', 'world', 'universe')");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ text_replace test failed: ${e.message}`);
    }

    // Test char_length
    console.log('\n13. Testing char_length()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT char_length('hello')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 5, "char_length('hello') = 5");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ char_length test failed: ${e.message}`);
    }

    // Test text_count
    console.log('\n14. Testing text_count()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT text_count('hello hello hello', 'hello')");
        lowLevel.step(stmt);
        assertEqual(lowLevel.columnInt(stmt, 0), 3, "text_count('hello hello hello', 'hello') = 3");
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ text_count test failed: ${e.message}`);
    }

    // Test sqlean_version
    console.log('\n15. Testing sqlean_version()\n');
    try {
        let stmt = lowLevel.prepare(db, "SELECT sqlean_version()");
        lowLevel.step(stmt);
        const version = lowLevel.columnText(stmt, 0);
        assert(version.includes('wasm'), `sqlean_version() = '${version}'`);
        lowLevel.finalize(stmt);
    } catch (e) {
        failed++;
        console.log(`  ✗ sqlean_version test failed: ${e.message}`);
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
