# text Extension

Text manipulation functions

## Source
- Type: sqlean
- Files: sqlite3-text.c, extension.c, bstring.c, rstring.c, runes.c, extension.h, bstring.h, rstring.h, runes.h

## Functions

| Function | Args | Type | Deterministic |
|----------|------|------|---------------|
| sqlean_version | 0 | scalar |  |
| text_substring | 2 | scalar |  |
| text_slice | 2 | scalar |  |
| text_left | 2 | scalar |  |
| left | 2 | scalar |  |
| text_right | 2 | scalar |  |
| right | 2 | scalar |  |
| text_index | 2 | scalar |  |
| strpos | 2 | scalar |  |
| text_last_index | 2 | scalar |  |
| text_contains | 2 | scalar |  |
| text_has_prefix | 2 | scalar |  |
| starts_with | 2 | scalar |  |
| text_has_suffix | 2 | scalar |  |
| text_count | 2 | scalar |  |
| text_like | 2 | scalar |  |
| text_split | 3 | scalar |  |
| split_part | 3 | scalar |  |
| text_join | -1 | scalar |  |
| concat_ws | -1 | scalar |  |
| text_concat | -1 | scalar |  |
| concat | -1 | scalar |  |
| text_repeat | 2 | scalar |  |
| repeat | 2 | scalar |  |
| text_ltrim | -1 | scalar |  |
| ltrim | -1 | scalar |  |
| text_rtrim | -1 | scalar |  |
| rtrim | -1 | scalar |  |
| text_trim | -1 | scalar |  |
| btrim | -1 | scalar |  |
| text_lpad | -1 | scalar |  |
| lpad | -1 | scalar |  |
| text_rpad | -1 | scalar |  |
| rpad | -1 | scalar |  |
| text_upper | 1 | scalar |  |
| text_lower | 1 | scalar |  |
| text_title | 1 | scalar |  |
| text_casefold | 1 | scalar |  |
| text_replace | 3 | scalar |  |
| text_translate | 3 | scalar |  |
| translate | 3 | scalar |  |
| text_reverse | 1 | scalar |  |
| reverse | 1 | scalar |  |
| text_length | 1 | scalar |  |
| char_length | 1 | scalar |  |
| character_length | 1 | scalar |  |
| text_size | 1 | scalar |  |
| octet_length | 1 | scalar |  |
| text_bitsize | 1 | scalar |  |
| bit_length | 1 | scalar |  |

## Usage

```javascript
import { lowLevel, extension } from '../build/js-ext/sqlite-extensible.js';
import { registerText } from './register.mjs';

const db = lowLevel.open(':memory:', { readwrite: true, create: true });
registerText(db, extension);

// Now you can use the extension functions in SQL
const stmt = lowLevel.prepare(db, "SELECT ...");
```

## Testing

```bash
node test-extension.mjs
```
