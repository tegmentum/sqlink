.load extensions/extfns/target/wasm32-wasip2/release/extfns_extension.component.wasm

/* charindex(haystack, needle, [start])  1-indexed match position.
 * 0 if not found. Matches extension-functions.c semantics. */
SELECT charindex('Hello, World!', 'World');
SELECT charindex('Hello, World!', 'l');
SELECT charindex('Hello, World!', 'l', 5);
SELECT charindex('Hello, World!', 'xyz');
SELECT charindex('Hello', '');

/* leftstr / rightstr  char-aware (not byte-aware). */
SELECT leftstr('Hello, World!', 5);
SELECT leftstr('Hello', 100);
SELECT coalesce(nullif(leftstr('Hello', 0), ''), '<empty>');
SELECT rightstr('Hello, World!', 6);
SELECT rightstr('Hello', 100);

/* reverse. */
SELECT reverse('Hello');
SELECT coalesce(nullif(reverse(''), ''), '<empty>');
SELECT reverse('a');

/* replicate. */
SELECT replicate('ab', 3);
SELECT replicate('-', 5);
SELECT coalesce(nullif(replicate('x', 0), ''), '<empty>');

/* proper: title-case each word. */
SELECT proper('hello world');
SELECT proper('HELLO WORLD');
SELECT proper('a quick brown fox');

/* pad family: harness strips leading whitespace (T-26), so we use
 * replace() to make the padding visible. */
SELECT replace(padl('42', 5), ' ', '.');
SELECT replace(padr('42', 5), ' ', '.');
SELECT replace(padc('42', 5), ' ', '.');

/* strfilter: keep only chars in allowed set. */
SELECT strfilter('Hello, World 123!', 'aeiou');
SELECT strfilter('user@example.com', 'abcdefghijklmnopqrstuvwxyz');
