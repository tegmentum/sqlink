.load extensions/roman/target/wasm32-wasip2/release/roman_extension.component.wasm

/* Roman numerals. Shared roman_encode/decode/validate plus the
 * to_roman/from_roman aliases gained from the datalink core superset.
 * Expected (in order): MCMXCIV, 1994, 1, 0, XLIX, 49, XLIX, 49. */
SELECT roman_encode(1994);
SELECT roman_decode('MCMXCIV');
SELECT roman_validate('MCMXCIV');
SELECT roman_validate('IIII');
SELECT roman_encode(49);
SELECT roman_decode('XLIX');
SELECT to_roman(49);
SELECT from_roman('XLIX');
