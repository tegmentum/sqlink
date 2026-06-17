-- Smoke test for the `morse` extension.
.load extensions/morse/target/wasm32-wasip2/release/morse_extension.component.wasm

SELECT morse_encode('SOS');
SELECT morse_encode('HELLO WORLD');
SELECT morse_decode('... --- ...');
SELECT morse_decode(morse_encode('Test Round Trip'));
