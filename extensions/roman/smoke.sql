-- Smoke test for the `roman` extension.
.load extensions/roman/target/wasm32-wasip2/release/roman_extension.component.wasm

SELECT roman_encode(1);
SELECT roman_encode(1994);
SELECT roman_encode(3999);
SELECT roman_encode(4000);
SELECT roman_decode('MCMXCIV');
SELECT roman_decode('iv');
SELECT roman_validate('MCMXCIV');
SELECT roman_validate('IIII');
