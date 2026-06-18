.load extensions/currency/target/wasm32-wasip2/release/currency_extension.component.wasm

/* Major currencies: name + numeric ISO 4217 code. */
SELECT currency_name('USD');
SELECT currency_numeric('USD');
SELECT currency_name('EUR');
SELECT currency_numeric('EUR');
SELECT currency_name('JPY');
SELECT currency_numeric('JPY');

/* Decimals: most are 2; JPY/KRW are 0; KWD/BHD/OMR/IQD/JOD are 3. */
SELECT currency_decimals('USD');
SELECT currency_decimals('JPY');     /* 0  no minor unit */
SELECT currency_decimals('KWD');     /* 3  fils */

/* Symbols  short, may be unicode. */
SELECT currency_symbol('GBP');
SELECT currency_symbol('USD');

/* Case-insensitive: 'usd' = 'USD'. */
SELECT currency_name('usd');

/* Fail-clean: unknown / malformed  NULL (rendered <NULL> via T-19). */
SELECT currency_name('XYZ');
SELECT currency_name('US');
SELECT currency_name('USDX');
