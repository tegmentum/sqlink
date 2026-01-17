/**
 * Register my-extension functions
 *
 * Usage:
 *   import { registerMyExtension } from './register.mjs';
 *   registerMyExtension(db, extension);
 */

import * as callbacks from './extension-callbacks.js';

/**
 * Register all my-extension functions with a database handle
 * @param {bigint} db - Database handle from lowLevel.open()
 * @param {object} extension - Extension API from sqlite-extensible
 * @returns {bigint[]} Array of function handles (for cleanup if needed)
 */
export function registerMyExtension(db, extension) {
    const handles = [];

    // Register my_function1
    // SQL: SELECT my_function1('hello')
    handles.push(extension.registerScalarFunction(
        db,
        'my_function1',       // Function name in SQL
        1,                    // Number of arguments (-1 for variadic)
        { deterministic: true },  // Function flags
        callbacks.FUNC_MY_FUNCTION1
    ));

    // Register my_function2
    // SQL: SELECT my_function2(10, 5)
    handles.push(extension.registerScalarFunction(
        db,
        'my_function2',
        2,
        { deterministic: true },
        callbacks.FUNC_MY_FUNCTION2
    ));

    // Example: Register an aggregate function
    // handles.push(extension.registerAggregateFunction(
    //     db,
    //     'my_sum',
    //     1,
    //     { deterministic: true },
    //     callbacks.FUNC_MY_SUM
    // ));

    // Example: Register a collation
    // handles.push(extension.registerCollation(
    //     db,
    //     'my_collation',
    //     callbacks.MY_COLLATION_ID
    // ));

    return handles;
}

/**
 * Unregister all functions (cleanup)
 * @param {object} extension - Extension API
 * @param {bigint[]} handles - Array of function handles from registerMyExtension
 */
export function unregisterMyExtension(extension, handles) {
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
    FUNC_MY_FUNCTION1,
    FUNC_MY_FUNCTION2
} from './extension-callbacks.js';
