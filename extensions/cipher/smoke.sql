.load extensions/cipher/target/wasm32-wasip2/release/cipher_extension.component.wasm

/* Caesar: classic +3 shift. */
SELECT caesar_encode('ABC', 3);              /* "DEF" */
SELECT caesar_encode('XYZ', 3);              /* "ABC" (wraps) */
SELECT caesar_encode('Hello, World!', 3);    /* preserves case + punct */

/* Decode = inverse shift. */
SELECT caesar_decode('DEF', 3);              /* "ABC" */
SELECT caesar_decode('Khoor, Zruog!', 3);    /* "Hello, World!" */

/* Negative shift = decode. */
SELECT caesar_encode('DEF', -3);             /* same as decode */

/* ROT13  self-inverse special case. */
SELECT rot13('Hello, World!');               /* "Uryyb, Jbeyq!" */
SELECT rot13(rot13('Hello, World!'));         /* identity */

/* Vigenère with classic example: "ATTACKATDAWN" + "LEMON". */
SELECT vigenere_encode('ATTACKATDAWN', 'LEMON');  /* "LXFOPVEFRNHR" */
SELECT vigenere_decode('LXFOPVEFRNHR', 'LEMON');  /* round-trip */

/* Vigenère skips non-letters but advances key pos only on letters. */
SELECT vigenere_encode('Hello, World!', 'KEY');

/* Empty / no-letter key  NULL. */
SELECT vigenere_encode('text', '');
SELECT vigenere_encode('text', '!!!');

/* Atbash: A<->Z, B<->Y. Self-inverse. */
SELECT atbash('HELLO');                       /* "SVOOL" */
SELECT atbash(atbash('Hello, World!'));        /* identity */

/* Shift 0 = identity. */
SELECT caesar_encode('Hello', 0);
