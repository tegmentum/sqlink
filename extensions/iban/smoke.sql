.load extensions/iban/target/wasm32-wasip2/release/iban_extension.component.wasm

/* Canonical example IBANs from the wikipedia "IBAN" examples list
 * each is a registry-valid test pattern, not a real account. */
SELECT iban_validate('GB82 WEST 1234 5698 7654 32');   /* 1  valid GB */
SELECT iban_validate('DE89 3704 0044 0532 0130 00');   /* 1  valid DE */
SELECT iban_validate('FR14 2004 1010 0505 0001 3M02 606'); /* 1  valid FR */

/* Country + check + bban decomposition. */
SELECT iban_country('GB82 WEST 1234 5698 7654 32');
SELECT iban_check_digits('GB82 WEST 1234 5698 7654 32');
SELECT iban_bban('GB82 WEST 1234 5698 7654 32');

/* Normalize: strip whitespace, uppercase. */
SELECT iban_normalize('  gb82 west 1234 5698 7654 32  ');

/* Format: groups of 4. */
SELECT iban_format('GB82WEST12345698765432');

/* Tamper one digit  check fails. */
SELECT iban_validate('GB83 WEST 1234 5698 7654 32');

/* Tamper one character  also fails. */
SELECT iban_validate('GB82 XEST 1234 5698 7654 32');

/* Wrong length for country  fails (DE = 22). */
SELECT iban_validate('DE89 3704 0044 0532 0130 0');

/* Unknown country  fails. */
SELECT iban_validate('XX00 1234 5678 9012 3456 78');

/* bban on invalid returns NULL. */
SELECT iban_bban('not an iban');

/* Empty input. */
SELECT iban_validate('');
