/**
 * My Extension Callbacks
 * Description of what this extension provides
 *
 * To create your own extension:
 * 1. Copy this template
 * 2. Update extension.json with your extension's metadata
 * 3. Define function IDs (use unique range to avoid conflicts)
 * 4. Implement your functions in onScalarFunction
 * 5. Package with: sqlite-wasm-ext package ./my-extension
 * 6. Publish to registry or distribute the .tar.gz
 */

// ============================================================================
// Function ID constants
// Use a unique range to avoid conflicts with other extensions
// Recommended: start at 1000+ for custom extensions
// ============================================================================
export const FUNC_MY_FUNCTION1 = 1000n;
export const FUNC_MY_FUNCTION2 = 1001n;

// ============================================================================
// Aggregate context storage (if you have aggregate functions)
// ============================================================================
const aggregateContexts = new Map();

// ============================================================================
// Helper functions for creating return values
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

// Helper to extract value from SqlValue argument
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
// This is called by SQLite when your function is invoked
// ============================================================================
export function onScalarFunction(functionId, args) {
    switch (functionId) {
        case FUNC_MY_FUNCTION1: {
            // Example: A function that takes one text argument
            // SQL: SELECT my_function1('hello')
            if (args.length < 1) {
                throw new Error('my_function1 requires 1 argument');
            }
            const text = getValue(args[0]);
            if (text === null) return makeNull();

            // Your implementation here
            const result = text.toUpperCase(); // Example: convert to uppercase

            return makeText(result);
        }

        case FUNC_MY_FUNCTION2: {
            // Example: A function that takes two arguments and returns an integer
            // SQL: SELECT my_function2(10, 5)
            if (args.length < 2) {
                throw new Error('my_function2 requires 2 arguments');
            }
            const a = getValue(args[0]);
            const b = getValue(args[1]);
            if (a === null || b === null) return makeNull();

            // Your implementation here
            const result = a + b; // Example: add two numbers

            return makeInteger(result);
        }

        default:
            throw new Error(`Unknown function id: ${functionId}`);
    }
}

// ============================================================================
// Aggregate function dispatchers (optional)
// Implement these if you have aggregate functions like SUM, AVG, etc.
// ============================================================================

/**
 * Called for each row during aggregate computation
 */
export function onAggregateStep(functionId, contextId, args) {
    switch (functionId) {
        // Add your aggregate functions here
        // Example:
        // case FUNC_MY_SUM: {
        //     let ctx = aggregateContexts.get(contextId);
        //     if (!ctx) {
        //         ctx = { total: 0 };
        //         aggregateContexts.set(contextId, ctx);
        //     }
        //     const val = getValue(args[0]);
        //     if (val !== null) ctx.total += val;
        //     break;
        // }

        default:
            throw new Error(`Unknown aggregate function id: ${functionId}`);
    }
}

/**
 * Called to get the final aggregate result
 */
export function onAggregateFinalize(functionId, contextId) {
    switch (functionId) {
        // Add your aggregate finalize here
        // Example:
        // case FUNC_MY_SUM: {
        //     const ctx = aggregateContexts.get(contextId);
        //     aggregateContexts.delete(contextId);
        //     return ctx ? makeFloat(ctx.total) : makeNull();
        // }

        default:
            throw new Error(`Unknown aggregate function id: ${functionId}`);
    }
}

// ============================================================================
// Collation dispatcher (optional)
// Implement if you want custom string comparison for ORDER BY
// ============================================================================
export function onCollationCompare(collationId, a, b) {
    switch (collationId) {
        // Add your collations here
        // Example: case-insensitive reverse comparison
        // case MY_COLLATION_ID: {
        //     const aLower = a.toLowerCase();
        //     const bLower = b.toLowerCase();
        //     if (bLower < aLower) return -1;
        //     if (bLower > aLower) return 1;
        //     return 0;
        // }

        default:
            // Default string comparison
            if (a < b) return -1;
            if (a > b) return 1;
            return 0;
    }
}

// ============================================================================
// Hook callbacks (optional)
// ============================================================================
const updateHooks = new Map();
const commitHooks = new Map();
const rollbackHooks = new Map();

export function onUpdate(hookId, op, database, table, rowid) {
    const hook = updateHooks.get(hookId);
    if (hook) hook(op, database, table, rowid);
}

export function onCommit(hookId) {
    const hook = commitHooks.get(hookId);
    if (hook) return hook();
    return false;
}

export function onRollback(hookId) {
    const hook = rollbackHooks.get(hookId);
    if (hook) hook();
}

export function onAuthorize(authId, action, arg1, arg2, database, trigger) {
    return 'ok'; // Allow by default
}

// ============================================================================
// Hook registration helpers (for advanced usage)
// ============================================================================
export function registerUpdateHook(hookId, callback) {
    updateHooks.set(hookId, callback);
}

export function registerCommitHook(hookId, callback) {
    commitHooks.set(hookId, callback);
}

export function registerRollbackHook(hookId, callback) {
    rollbackHooks.set(hookId, callback);
}
