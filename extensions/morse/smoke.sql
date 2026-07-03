-- Smoke test for the `morse` extension.
.load morse

SELECT morse_encode('SOS');
SELECT morse_encode('HELLO WORLD');
SELECT morse_decode('... --- ...');
SELECT morse_decode(morse_encode('Test Round Trip'));
