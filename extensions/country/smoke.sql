.load extensions/country/target/wasm32-wasip2/release/country_extension.component.wasm

/* alpha-2 input. */
SELECT country_name('US');
SELECT country_alpha3('US');
SELECT country_numeric('US');
SELECT country_region('US');

/* alpha-3 input  same answers. */
SELECT country_name('DEU');
SELECT country_alpha2('DEU');

/* Numeric input  same. */
SELECT country_name('392');     /* Japan */
SELECT country_alpha2('392');

/* Region coverage. */
SELECT country_region('JP');
SELECT country_region('NG');
SELECT country_region('BR');
SELECT country_region('AU');

/* Case-insensitive: 'gb' = 'GB'. */
SELECT country_name('gb');

/* Fail-clean: unknown or malformed  NULL. */
SELECT country_name('XX');
SELECT country_name('not a code');
SELECT country_name('9999');
SELECT country_name('');
