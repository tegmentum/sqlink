# SQLite WASM Extension Template

This is a template for creating SQLite WASM extensions that work with the sqlite-wasm component model.

## Quick Start

1. **Copy this template**
   ```bash
   cp -r registry/extension-template my-extension
   cd my-extension
   ```

2. **Update extension.json**
   - Set your extension name, version, description
   - List the functions you'll export
   - Choose a unique function ID range (1000+ recommended)

3. **Implement your functions**
   - Edit `extension-callbacks.js`
   - Add your function IDs
   - Implement the functions in `onScalarFunction`

4. **Update register.mjs**
   - Add registration calls for each function
   - Set correct argument counts and flags

5. **Test locally**
   ```javascript
   import { registerMyExtension } from './register.mjs';
   import { lowLevel, extension } from 'sqlite-extensible.js';

   const db = lowLevel.open(':memory:', { readwrite: true, create: true, memory: true });
   registerMyExtension(db, extension);

   const stmt = lowLevel.prepare(db, "SELECT my_function1('hello')");
   lowLevel.step(stmt);
   console.log(lowLevel.columnText(stmt, 0)); // HELLO
   ```

6. **Package for distribution**
   ```bash
   sqlite-wasm-ext package ./my-extension -o my-extension-0.1.0.tar.gz
   ```

## File Structure

```
my-extension/
├── extension.json           # Extension metadata
├── extension-callbacks.js   # Function implementations
├── register.mjs             # Registration helpers
└── README.md                # Documentation
```

## Function Types

### Scalar Functions
Return a single value for each row.

```javascript
case FUNC_MY_FUNC: {
    const arg = getValue(args[0]);
    return makeText(arg.toUpperCase());
}
```

### Aggregate Functions
Accumulate values across rows.

```javascript
// Step: called for each row
case FUNC_MY_SUM: {
    let ctx = aggregateContexts.get(contextId) || { total: 0 };
    ctx.total += getValue(args[0]) || 0;
    aggregateContexts.set(contextId, ctx);
    break;
}

// Finalize: return final result
case FUNC_MY_SUM: {
    const ctx = aggregateContexts.get(contextId);
    aggregateContexts.delete(contextId);
    return makeFloat(ctx?.total || 0);
}
```

### Collations
Custom string comparison for ORDER BY.

```javascript
case MY_COLLATION: {
    return a.localeCompare(b, 'en', { sensitivity: 'base' });
}
```

## Return Types

| Helper | SQLite Type | JavaScript Input |
|--------|-------------|------------------|
| `makeNull()` | NULL | - |
| `makeInteger(n)` | INTEGER | number or bigint |
| `makeFloat(n)` | REAL | number |
| `makeText(s)` | TEXT | string |
| `makeBlob(b)` | BLOB | Uint8Array |

## Function Flags

| Flag | Description |
|------|-------------|
| `deterministic: true` | Same inputs always produce same output |
| `directOnly: true` | Can only be called directly, not in triggers/views |

## Function ID Ranges

To avoid conflicts, use these ID ranges:

| Range | Usage |
|-------|-------|
| 1-99 | Core/built-in |
| 100-999 | Official extensions (text, uuid, etc.) |
| 1000-9999 | Community extensions |
| 10000+ | Private/custom extensions |

## Publishing

1. Create a GitHub repository for your extension
2. Add to the registry by submitting a PR to add your extension to `registry/index.json`
3. Or distribute the .tar.gz file directly

## Testing

Create a test file:

```javascript
// test.mjs
import { lowLevel, extension } from '../build/js-ext/sqlite-extensible.js';
import { registerMyExtension } from './register.mjs';

const db = lowLevel.open(':memory:', { readwrite: true, create: true, memory: true });
registerMyExtension(db, extension);

// Test your functions
const stmt = lowLevel.prepare(db, "SELECT my_function1('test')");
lowLevel.step(stmt);
console.assert(lowLevel.columnText(stmt, 0) === 'TEST', 'my_function1 failed');
lowLevel.finalize(stmt);

console.log('All tests passed!');
lowLevel.close(db);
```

Run:
```bash
node test.mjs
```
