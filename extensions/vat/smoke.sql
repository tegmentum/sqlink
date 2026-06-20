.load extensions/vat/target/wasm32-wasip2/release/vat_extension.component.wasm

/* ─── vat_normalize: upper + strip ws/punct ─── */
SELECT vat_normalize(' de 123 456 789 ');
SELECT vat_normalize('FR.40.303.265.045');
SELECT vat_normalize('xx');                        -- too short → NULL
SELECT vat_normalize('1234');                       -- no alpha prefix → NULL

/* ─── vat_country: alpha-2 prefix ─── */
SELECT vat_country('de136695976');
SELECT vat_country('IT07643520567');
SELECT vat_country('xx');                          -- bad shape → NULL

/* ─── vat_is_valid: published worked examples ─── */
/* AT BMF doc example */
SELECT vat_is_valid('ATU13585627');
SELECT vat_is_valid('ATU13585626');
/* BE example: 0123456749 → mod-97 */
SELECT vat_is_valid('BE0123456749');
/* DE BMF ISO 7064 worked example */
SELECT vat_is_valid('DE136695976');
SELECT vat_is_valid('DE136695977');
/* FR EU VIES doc example */
SELECT vat_is_valid('FR40303265045');
/* IT official IT VAT doc example (Luhn) */
SELECT vat_is_valid('IT07643520567');
SELECT vat_is_valid('IT07643520568');
/* NL mod 11 with weights 9..2 */
SELECT vat_is_valid('NL004495445B01');
/* GB mod-97 */
SELECT vat_is_valid('GB123456782');
SELECT vat_is_valid('GB123456789');
/* Norwegian Brønnøysund test number */
SELECT vat_is_valid('NO974760673');
SELECT vat_is_valid('NO974760673MVA');
/* mixed-case + punctuation tolerated */
SELECT vat_is_valid(' de-136 695 976 ');
/* unsupported country prefix */
SELECT vat_is_valid('XX12345');

/* ─── vat_country_supported ─── */
SELECT vat_country_supported('DE');
SELECT vat_country_supported('fr');
SELECT vat_country_supported('XX');
SELECT vat_country_supported('US');
SELECT vat_country_supported('NO');
SELECT vat_country_supported('CH');

/* ─── vat_supported_countries: JSON array, contains DE + UK + NO ─── */
SELECT json_valid(vat_supported_countries());
SELECT instr(vat_supported_countries(), '"DE"') > 0;
SELECT instr(vat_supported_countries(), '"NO"') > 0;
SELECT instr(vat_supported_countries(), '"GB"') > 0;

/* ─── NULL → NULL on every text scalar ─── */
SELECT vat_is_valid(NULL);
SELECT vat_country(NULL);
SELECT vat_normalize(NULL);
SELECT vat_country_supported(NULL);

/* ─── version non-empty ─── */
SELECT length(vat_version()) > 0;
