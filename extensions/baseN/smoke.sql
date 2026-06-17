-- Smoke test for the `baseN` extension.
.load extensions/baseN/target/wasm32-wasip2/release/baseN_extension.component.wasm

SELECT base32_encode(x'48656c6c6f');
SELECT hex(base32_decode('JBSWY3DP'));
SELECT base58_encode(x'0001020304');
SELECT hex(base58_decode('12VfUX'));
SELECT base58_decode('invalid char!');
