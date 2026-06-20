.load extensions/iban/target/wasm32-wasip2/release/iban_extension.component.wasm

/* Canonical example IBANs from the wikipedia "IBAN" examples list;
 * each is a registry-valid test pattern, not a real account. */
SELECT iban_is_valid('GB82 WEST 1234 5698 7654 32');   /* 1 valid GB */
SELECT iban_is_valid('DE89 3704 0044 0532 0130 00');   /* 1 valid DE */
SELECT iban_is_valid('FR14 2004 1010 0505 0001 3M02 606'); /* 1 valid FR */

/* Country + check + bban decomposition (DE example, plan acceptance). */
SELECT iban_country('DE89370400440532013000');
SELECT iban_check_digits('DE89370400440532013000');
SELECT iban_bban('DE89370400440532013000');

/* Per-country bank code + account number from BBAN. DE = 8 bank
 * code digits (37040044 = Deutsche Bank Frankfurt), then 10 acct
 * digits (0532013000). */
SELECT iban_bank_code('DE89370400440532013000');
SELECT iban_account_number('DE89370400440532013000');

/* GB layout = 4-char bank code (WEST = NatWest), 6 sort code, 8 acct. */
SELECT iban_bank_code('GB82WEST12345698765432');
SELECT iban_account_number('GB82WEST12345698765432');

/* Normalize: strip whitespace, uppercase (plan acceptance). */
SELECT iban_normalize('  de89 3704 0044 0532 0130 00  ');

/* Format: groups of 4 (plan acceptance). */
SELECT iban_format('DE89370400440532013000');

/* Tamper one digit -> mod-97 fails. */
SELECT iban_is_valid('GB83 WEST 1234 5698 7654 32');

/* Tamper one character -> fails. */
SELECT iban_is_valid('GB82 XEST 1234 5698 7654 32');

/* Wrong length for country -> fails (DE = 22). */
SELECT iban_is_valid('DE89 3704 0044 0532 0130 0');

/* Unknown country -> fails. */
SELECT iban_is_valid('XX00 1234 5678 9012 3456 78');

/* bban on invalid returns NULL. */
SELECT iban_bban('not an iban');

/* Empty input. */
SELECT iban_is_valid('');

/* NULL -> NULL propagation across the surface. */
SELECT iban_is_valid(NULL);
SELECT iban_bank_code(NULL);

/* Version is a literal. */
SELECT iban_version();
