.load extensions/vcard/target/wasm32-wasip2/release/vcard_extension.component.wasm

/* ─── vCard 3.0 fixture (RFC 2426) ───
 * One canonical contact with every property the plan calls out:
 * FN, N, EMAIL (one), TEL, ORG, TITLE, ADR, BDAY, URL, NOTE. */
SELECT vcard_fn(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
N:Smith;Jane;;;
EMAIL:jane@example.com
TEL:+1-555-2000
ORG:Example Co;R&D
TITLE:Researcher
ADR:;;456 Oak Ave;Anytown;NY;10001;USA
BDAY:1990-04-12
URL:https://janesmith.example.org
NOTE:Test contact
END:VCARD
');

SELECT vcard_email(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
EMAIL:jane@example.com
END:VCARD
');

SELECT vcard_phone(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
TEL:+1-555-2000
END:VCARD
');

SELECT vcard_org(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
ORG:Example Co;R&D
END:VCARD
');

SELECT vcard_title(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
TITLE:Researcher
END:VCARD
');

SELECT vcard_birthday(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
BDAY:1990-04-12
END:VCARD
');

SELECT vcard_url(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
URL:https://janesmith.example.org
END:VCARD
');

SELECT vcard_note(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
NOTE:Test contact
END:VCARD
');

SELECT vcard_addresses(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
ADR:;;456 Oak Ave;Anytown;NY;10001;USA
END:VCARD
');

SELECT vcard_version_in(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
END:VCARD
');

/* ─── vCard 4.0 fixture (RFC 6350) ───
 * Compact basic-format BDAY (19850615  1985-06-15 ISO extended). */
SELECT vcard_fn(
'BEGIN:VCARD
VERSION:4.0
FN:John Doe
EMAIL:john@example.com
TEL:+1-555-1234
ORG:Acme Inc
TITLE:Engineer
BDAY:19850615
URL:https://example.com
NOTE:Some note
END:VCARD
');

SELECT vcard_birthday(
'BEGIN:VCARD
VERSION:4.0
FN:John Doe
BDAY:19850615
END:VCARD
');

SELECT vcard_version_in(
'BEGIN:VCARD
VERSION:4.0
FN:John Doe
END:VCARD
');

/* ─── Multi-EMAIL: vcard_emails returns every value, vcard_email
 * returns just the first. RFC 6350  6.4.2. */
SELECT vcard_emails(
'BEGIN:VCARD
VERSION:4.0
FN:Multi Person
EMAIL:first@example.com
EMAIL;TYPE=work:work@example.com
EMAIL;TYPE=home:home@example.com
END:VCARD
');

SELECT vcard_email(
'BEGIN:VCARD
VERSION:4.0
FN:Multi Person
EMAIL:first@example.com
EMAIL;TYPE=work:work@example.com
EMAIL;TYPE=home:home@example.com
END:VCARD
');

/* ─── Multi-TEL ─── */
SELECT vcard_phones(
'BEGIN:VCARD
VERSION:4.0
FN:Multi Phone
TEL:+1-555-1111
TEL;TYPE=work:+1-555-2222
END:VCARD
');

/* ─── Multi-vcard input: returns the first card's fields. ─── */
SELECT vcard_fn(
'BEGIN:VCARD
VERSION:3.0
FN:First Person
END:VCARD
BEGIN:VCARD
VERSION:3.0
FN:Second Person
END:VCARD
');

/* ─── vcard_all packs every populated field into a JSON object.
 * json_extract gets the version  acceptance-style query. */
SELECT json_extract(vcard_all(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
EMAIL:jane@example.com
END:VCARD
'), '$.version');

SELECT json_extract(vcard_all(
'BEGIN:VCARD
VERSION:3.0
FN:Jane Smith
EMAIL:jane@example.com
END:VCARD
'), '$.fn');

/* ─── Random text  every scalar NULL. ─── */
SELECT vcard_fn('just random text not a vcard');
SELECT vcard_email('just random text not a vcard');
SELECT vcard_phone('just random text not a vcard');
SELECT vcard_birthday('just random text not a vcard');
SELECT vcard_addresses('just random text not a vcard');

/* ─── NULL input  NULL out, no error. ─── */
SELECT vcard_fn(NULL);

/* ─── Version is non-empty. ─── */
SELECT length(vcard_version()) > 0;
