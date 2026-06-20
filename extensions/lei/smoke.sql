.load extensions/lei/target/wasm32-wasip2/release/lei_extension.component.wasm

/* Canonical published LEIs (GLEIF). Each is 20 alphanumeric chars
 * and satisfies ISO 7064 MOD 97-10  is_valid -> 1. */
SELECT lei_is_valid('529900T8BM49AURSDO55');
SELECT lei_is_valid('5493001KJTIIGC8Y1R12');
SELECT lei_is_valid('HWUPKR0MPOU8FGXBT394');
SELECT lei_is_valid('7LTWFZYICNSX8D621K86');

/* Lower-case + spaces + hyphens -> normalize strips/uppercases. */
SELECT lei_normalize('  529900t8bm49-aursdo55  ');

/* is_valid runs on the normalized form, so display-form is accepted. */
SELECT lei_is_valid('529900T8BM49-AURSDO55');
SELECT lei_is_valid('529900 T8BM 49AU RSDO 55');

/* Check digits = last 2 chars after normalize. */
SELECT lei_check_digits('529900T8BM49AURSDO55');
SELECT lei_check_digits('5493001KJTIIGC8Y1R12');

/* Tamper a check digit -> mod-97 fails. */
SELECT lei_is_valid('529900T8BM49AURSDO56');

/* Tamper a body character -> fails. */
SELECT lei_is_valid('529900T8BX49AURSDO55');

/* Wrong length (19 chars) -> fails. */
SELECT lei_is_valid('529900T8BM49AURSDO5');

/* Wrong length (21 chars) -> fails. */
SELECT lei_is_valid('529900T8BM49AURSDO555');

/* Non-alphanumeric body char (after separator strip) -> fails. */
SELECT lei_is_valid('529900T8BM49AURSDO5!');

/* Empty input. */
SELECT lei_is_valid('');

/* NULL -> NULL propagation. */
SELECT lei_is_valid(NULL);
SELECT lei_normalize(NULL);
SELECT lei_check_digits(NULL);

/* Version is a literal. */
SELECT lei_version();
