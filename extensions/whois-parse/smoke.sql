.load extensions/whois-parse/target/wasm32-wasip2/release/whois_parse_extension.component.wasm

/* ---- baseline Verisign-style .com response (key:value shape) ----
 * Lifted shape from a typical example.com response, trimmed to the
 * fields the parser surfaces. Multi-NS to exercise dedupe + order. */

/* whois_registrar  picks up "Registrar:" verbatim. */
SELECT whois_registrar(
  'Domain Name: EXAMPLE.COM' || char(10) ||
  'Registrar: ICANN ACME REGISTRAR' || char(10) ||
  'Creation Date: 1995-08-14T04:00:00Z' || char(10) ||
  'Registry Expiry Date: 2026-08-13T04:00:00Z' || char(10) ||
  'Name Server: NS1.EXAMPLE.COM' || char(10) ||
  'Name Server: NS2.EXAMPLE.COM' || char(10));

/* whois_creation_date  ISO-prefixed datetime  YYYY-MM-DD. */
SELECT whois_creation_date(
  'Domain Name: EXAMPLE.COM' || char(10) ||
  'Registrar: ICANN ACME REGISTRAR' || char(10) ||
  'Creation Date: 1995-08-14T04:00:00Z' || char(10) ||
  'Registry Expiry Date: 2026-08-13T04:00:00Z' || char(10));

/* whois_expiration_date  same date normalisation path. */
SELECT whois_expiration_date(
  'Domain Name: EXAMPLE.COM' || char(10) ||
  'Registry Expiry Date: 2026-08-13T04:00:00Z' || char(10));

/* whois_name_servers  JSON array, lowercased, order preserved,
 * trailing dot stripped, dupe collapsed. */
SELECT whois_name_servers(
  'Name Server: NS1.EXAMPLE.COM.' || char(10) ||
  'Name Server: NS2.EXAMPLE.COM' || char(10) ||
  'Name Server: NS1.EXAMPLE.COM' || char(10));

/* whois_field is case-insensitive on the field-name lookup. */
SELECT whois_field(
  'Registrar: ICANN ACME REGISTRAR' || char(10) ||
  'Creation Date: 1995-08-14T04:00:00Z',
  'registrar');

/* ---- date normalisations ----
 * DD-Mon-YYYY (Verisign legacy)  ISO. */
SELECT whois_creation_date(
  'Created On: 15-Apr-2023' || char(10));

/* YYYY.MM.DD dotted (some ccTLDs)  ISO. */
SELECT whois_expiration_date(
  'Expires: 2024.06.01' || char(10));

/* DD.MM.YYYY (DENIC, .ru)  ISO. */
SELECT whois_creation_date(
  'Registered On: 03.11.2020' || char(10));

/* Unparseable date falls through verbatim. */
SELECT whois_creation_date(
  'Creation Date: not-a-date-at-all' || char(10));

/* ---- shape tolerance: key=value (older GoDaddy / ccTLD) ---- */
SELECT whois_registrar(
  'Domain Name=EXAMPLE.NET' || char(10) ||
  'Registrar=Older Registrar Inc' || char(10));

/* ---- shape tolerance: whitespace-separated (ARIN-ish) ----
 * Two+ spaces between key and value. whois_registrar doesn't
 * synonym-map ARIN's "OrgName" (an IP-block owner, not a domain
 * registrar), so we exercise shape-3 via whois_field instead. */
SELECT whois_field(
  'OrgName        ACME Network Services' || char(10) ||
  'Country        US' || char(10),
  'OrgName');

/* ---- RIPE-style: % comment lines are ignored ---- */
SELECT whois_field(
  '% This is the RIPE Database query service.' || char(10) ||
  '% The objects are in RPSL format.' || char(10) ||
  'inetnum:        192.0.2.0 - 192.0.2.255' || char(10) ||
  'netname:        EXAMPLE-NET' || char(10),
  'netname');

/* ---- APNIC sponsoring-registrar synonym is recognised ---- */
SELECT whois_registrar(
  'Sponsoring Registrar: APNIC Hostmaster' || char(10));

/* ---- empty / no-match cases  NULL ---- */
SELECT whois_registrar('no registrar field here' || char(10));
SELECT whois_field('Registrar: X' || char(10), 'creation date');

/* ---- name_servers empty input  empty JSON array ---- */
SELECT whois_name_servers('no NS records here' || char(10));

/* ---- whois_parse  JSON object round-trip via json_extract ----
 * Parses 'Domain Name: EXAMPLE.COM\nRegistrar: ACME\n' and pulls
 * back the Registrar value via SQLite's built-in json_extract. */
SELECT json_extract(
  whois_parse('Domain Name: EXAMPLE.COM' || char(10) || 'Registrar: ACME' || char(10)),
  '$.Registrar');

/* ---- NULL propagation across all scalars ---- */
SELECT whois_field(NULL, 'registrar');
SELECT whois_field('Registrar: X', NULL);
SELECT whois_registrar(NULL);
SELECT whois_creation_date(NULL);
SELECT whois_expiration_date(NULL);
SELECT whois_name_servers(NULL);
SELECT whois_parse(NULL);

/* ---- version is non-empty ---- */
SELECT length(whois_version()) > 0;
