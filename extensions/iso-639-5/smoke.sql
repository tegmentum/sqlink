.load extensions/iso-639-5/target/wasm32-wasip2/release/iso_639_5_extension.component.wasm

/* ---- iso639_5_name ---- */
SELECT iso639_5_name('afa');              /* Afro-Asiatic languages */
SELECT iso639_5_name('sla');              /* Slavic languages */
SELECT iso639_5_name('ine');              /* Indo-European languages */
SELECT iso639_5_name('roa');              /* Romance languages */
SELECT iso639_5_name('SLA');              /* case-insensitive -> Slavic languages */
SELECT iso639_5_name('  sla  ');          /* whitespace trimmed -> Slavic languages */
SELECT iso639_5_name('xxx');              /* unknown -> NULL */
SELECT iso639_5_name('xx');               /* wrong length -> NULL */
SELECT iso639_5_name(NULL);               /* NULL -> NULL */

/* ---- iso639_5_is_valid ---- */
SELECT iso639_5_is_valid('sla');          /* 1 */
SELECT iso639_5_is_valid('SLA');          /* 1 (case-insensitive) */
SELECT iso639_5_is_valid('zzz');          /* 0 */
SELECT iso639_5_is_valid('en');           /* 0 (639-1, not a family code) */

/* ---- iso639_5_list ---- */
SELECT json_array_length(iso639_5_list());                                    /* 115 */
SELECT json_extract(iso639_5_list(), '$[0].code');                            /* aav (alphabetically first) */
SELECT json_extract(iso639_5_list(), '$[1].code');                            /* afa */
SELECT json_extract(iso639_5_list(), '$[1].name');                            /* Afro-Asiatic languages */

/* ---- iso639_5_version ---- */
SELECT iso639_5_version() LIKE 'iso-639-5 %ISO 639-5%';   /* 1 */
