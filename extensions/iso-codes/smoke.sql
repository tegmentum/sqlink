.load extensions/iso-codes/target/wasm32-wasip2/release/iso_codes_extension.component.wasm

/* ---- ISO 3166-1 country ---- */
SELECT iso3166_alpha2_name('US');                /* United States */
SELECT iso3166_alpha3_name('USA');               /* United States */
SELECT iso3166_alpha2_to_alpha3('US');           /* USA */
SELECT iso3166_alpha3_to_alpha2('DEU');          /* DE */
SELECT iso3166_numeric('US');                    /* 840 */
SELECT iso3166_numeric('DEU');                   /* 276  alpha-3 also accepted */
SELECT iso3166_is_valid('US');                   /* 1 */
SELECT iso3166_is_valid('XX');                   /* 0 */
SELECT iso3166_alpha2_name('us');                /* case-insensitive  United States */
SELECT iso3166_alpha2_name('zz');                /* unknown  NULL */
SELECT iso3166_alpha2_name(NULL);                /* NULL  NULL */

/* ---- ISO 4217 currency ---- */
SELECT iso4217_name('USD');                      /* US Dollar */
SELECT iso4217_minor_units('USD');               /* 2 */
SELECT iso4217_minor_units('JPY');               /* 0  yen has no minor unit */
SELECT iso4217_minor_units('BHD');               /* 3  Bahraini Dinar */
SELECT iso4217_is_valid('USD');                  /* 1 */
SELECT iso4217_is_valid('XYZ');                  /* 0 */
SELECT iso4217_name('usd');                      /* case-insensitive  US Dollar */
SELECT iso4217_name('ZZZ');                      /* unknown  NULL */

/* ---- ISO 639 language ---- */
SELECT iso639_alpha2_name('en');                 /* English */
SELECT iso639_alpha3_name('eng');                /* English */
SELECT iso639_alpha2_to_alpha3('en');            /* eng */
SELECT iso639_alpha3_to_alpha2('eng');           /* en */
SELECT iso639_is_valid('en');                    /* 1 */
SELECT iso639_is_valid('xx');                    /* 0 */
SELECT iso639_alpha2_name('EN');                 /* case-insensitive  English */
SELECT iso639_alpha2_name('zz');                 /* unknown  NULL */
