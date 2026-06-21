.load extensions/stdsql/target/wasm32-wasip2/release/stdsql_extension.component.wasm

/* greatest / least  variadic, NULL-skipping. */
SELECT greatest(1, 5, 3, 2);
SELECT least(1, 5, 3, 2);
SELECT greatest('apple', 'banana', 'cherry');
SELECT least('apple', 'banana', 'cherry');
SELECT greatest(NULL, 1, NULL, 4, 2);
SELECT coalesce(greatest(NULL, NULL), '(all null)');

/* left / right  char-aware. */
SELECT left('Hello, World!', 5);
SELECT right('Hello, World!', 6);
SELECT left('ABCDE', 100);
SELECT left('', 5);

/* lpad / rpad. */
SELECT lpad('abc', 7);
SELECT lpad('abc', 7, '*');
SELECT rpad('abc', 7);
SELECT rpad('abc', 7, '-=');
SELECT lpad('abcdef', 3);

/* repeat / space. */
SELECT repeat('ab', 3);
SELECT '[' || space(5) || ']';

/* starts_with / ends_with. */
SELECT starts_with('Hello, World', 'Hello');
SELECT starts_with('Hello, World', 'World');
SELECT ends_with('Hello, World', 'World');
SELECT ends_with('Hello, World', 'Hello');

/* translate. */
SELECT translate('Hello', 'el', 'ip');
SELECT translate('12345', '13', 'AB');
SELECT translate('abcdef', 'abc', 'X');

/* to_hex / from_hex. */
SELECT to_hex(255);
SELECT to_hex(0);
SELECT to_hex(65535);
SELECT hex(from_hex('48656c6c6f'));

/* bit_length / char_length / character_length. */
SELECT bit_length('abc');
SELECT char_length('hello');
SELECT character_length('');

/* initcap. */
SELECT initcap('hello world');
SELECT initcap('HELLO WORLD');

/* if alias for iif. */
SELECT if(1, 'yes', 'no');
SELECT if(0, 'yes', 'no');
SELECT if(NULL, 'yes', 'no');

/* chr / ascii. */
SELECT chr(65);
SELECT ascii('A');
SELECT chr(0x1F600);
