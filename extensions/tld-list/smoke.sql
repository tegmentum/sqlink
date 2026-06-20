.load extensions/tld-list/target/wasm32-wasip2/release/tld_list_extension.component.wasm

/* ─── tld_type: classify a TLD ─── */
SELECT tld_type('com');
SELECT tld_type('uk');
SELECT tld_type('gov');
SELECT tld_type('arpa');
SELECT tld_type('test');

/* ─── case-insensitivity + leading-dot stripping ─── */
SELECT tld_type('COM');
SELECT tld_type('.com');
SELECT tld_type('.UK');

/* ─── tld_is_valid: 1 for known, 0 for unknown but well-shaped ─── */
SELECT tld_is_valid('com');
SELECT tld_is_valid('zzznotreal');

/* ─── tld_country: ISO 3166 alpha-2 for cctld, NULL otherwise ─── */
SELECT tld_country('uk');
SELECT tld_country('jp');
SELECT tld_country('de');
SELECT tld_country('com');
SELECT tld_country('gov');

/* ─── tld_punycode: same string for ASCII, xn-- for IDN Unicode ─── */
SELECT tld_punycode('com');
SELECT tld_punycode('xn--p1ai');
SELECT tld_punycode('рф');

/* ─── tld_extract: last label of a domain, lowercased ─── */
SELECT tld_extract('www.example.com');
SELECT tld_extract('EXAMPLE.CO.UK');
SELECT tld_extract('a.b.c.example.org');
SELECT tld_extract('singlelabel');
SELECT tld_extract('example.com.');

/* ─── tld_list: JSON array, non-empty, starts with '[' ─── */
SELECT substr(tld_list(), 1, 1);
SELECT length(tld_list()) > 100;

/* ─── tld_list_version: non-empty version string ─── */
SELECT length(tld_list_version()) > 0;

/* ─── NULL input  NULL output on every scalar ─── */
SELECT tld_type(NULL);
SELECT tld_is_valid(NULL);
SELECT tld_country(NULL);
SELECT tld_punycode(NULL);
SELECT tld_extract(NULL);

/* ─── empty / garbage input  NULL ─── */
SELECT tld_type('');
SELECT tld_extract('   ');
SELECT tld_extract('...');
