.load extensions/publicsuffix/target/wasm32-wasip2/release/publicsuffix_extension.component.wasm

/* ─── psl_tld: extract the public suffix (eTLD) ─── */
SELECT psl_tld('www.example.co.uk');
SELECT psl_tld('example.com');
SELECT psl_tld('a.b.c.example.org');

/* ─── psl_etld1: extract the registrable (eTLD+1) domain ─── */
SELECT psl_etld1('www.example.co.uk');
SELECT psl_etld1('api.subdomain.example.com');
SELECT psl_etld1('example.com');

/* ─── psl_is_public: 1 if input == its own public suffix ─── */
SELECT psl_is_public('co.uk');
SELECT psl_is_public('com');
SELECT psl_is_public('example.com');
SELECT psl_is_public('www.example.com');

/* ─── psl_subdomain: labels left of the eTLD+1 ─── */
/* Wrap with '|...|' delimiters so the empty-string result for the
 * "no subdomain" case survives smoke.py's blank-line filter. */
SELECT '|' || psl_subdomain('www.example.com') || '|';
SELECT '|' || psl_subdomain('example.com') || '|';
SELECT '|' || psl_subdomain('a.b.example.co.uk') || '|';
SELECT '|' || psl_subdomain('deep.nested.subdomain.example.org') || '|';

/* ─── case folding: PSL labels are case-insensitive ─── */
SELECT psl_etld1('WWW.Example.COM');

/* ─── trailing dot (FQDN form) is tolerated ─── */
SELECT psl_etld1('www.example.com.');
SELECT psl_tld('www.example.com.');

/* ─── NULL input  NULL output on every scalar ─── */
SELECT psl_tld(NULL);
SELECT psl_etld1(NULL);
SELECT psl_is_public(NULL);
SELECT psl_subdomain(NULL);

/* ─── empty / all-dots input  NULL (not error) ─── */
SELECT psl_tld('');
SELECT psl_etld1('...');

/* ─── version is non-empty ─── */
SELECT length(publicsuffix_version()) > 0;
