.load extensions/nato-phonetic/target/wasm32-wasip2/release/nato_phonetic_extension.component.wasm

/* Encode single words. */
SELECT nato_encode('ABC');
SELECT nato_encode('SOS');
SELECT nato_encode('hello');

/* Encode multi-word: " | " separator. */
SELECT nato_encode('AB CD');

/* Encode mixed alphanumeric. */
SELECT nato_encode('A1B2');

/* Per-letter lookup. */
SELECT nato_word('A');
SELECT nato_word('z');
SELECT nato_word('7');
SELECT nato_word('!');

/* Decode  case-insensitive. */
SELECT nato_decode('Alpha Bravo Charlie');
SELECT nato_decode('alpha bravo charlie');

/* Decode multi-word: "|" boundary  space. */
SELECT nato_decode('Alpha Bravo | Charlie Delta');

/* Round-trip property: decode(encode(x))  upper(x without ws). */
SELECT nato_decode(nato_encode('Hello World'));

/* Decode with unknown words  best-effort first-char fallback. */
SELECT nato_decode('Apple Banana Cherry');
