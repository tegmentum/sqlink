.load extensions/xor/target/wasm32-wasip2/release/xor_extension.component.wasm

/* Single-byte XOR. 'A' (0x41) XOR 'K' (0x4B) = 0x0A. */
SELECT xor_encode('A', 'K');             /* "0a" */

/* Multi-byte text + repeating key. */
SELECT xor_encode('Hello', 'k');         /* repeating key 'k' */
SELECT xor_encode('Hello', 'key');       /* key cycles */

/* Round-trip property: decode(encode(x, k), k)  x. */
SELECT xor_decode(xor_encode('Hello, World!', 'secret'), 'secret');

/* Composition: encode  encode treats the inner hex as text, so
 * this is NOT a self-inverse  it locks the actual behavior. */
SELECT xor_decode(xor_encode(xor_encode('Hello', 'k'), 'k'), 'k');

/* Cipher property: same key + same input = same output (deterministic). */
SELECT xor_encode('test', 'key') = xor_encode('test', 'key');

/* Decode hex with wrong key  produces gibberish (still text or blob). */
SELECT xor_decode(xor_encode('Hello', 'k'), 'X') = 'Hello';   /* 0  wrong key */

/* Fail-clean cases. */
SELECT xor_encode('text', '');           /* empty key  NULL */
SELECT xor_decode('not hex', 'k');       /* malformed hex  NULL */
SELECT xor_decode('abc', 'k');           /* odd hex length  NULL */
