.load extensions/asn1/target/wasm32-wasip2/release/asn1_extension.component.wasm

/* ---- OID lookup ----
 * Curated table covers the standard crypto + RDN OIDs the plan
 * calls out by name. */
SELECT asn1_oid_name('1.2.840.113549.1.1.11');
SELECT asn1_oid_name('2.5.4.3');
SELECT asn1_oid_name('1.3.101.112');
SELECT asn1_oid_name('1.2.840.10045.2.1');
SELECT asn1_oid_name('2.16.840.1.101.3.4.2.1');

/* Unknown OID -> NULL (not an error). */
SELECT asn1_oid_name('1.2.3.4.5.99');

/* Reverse lookup: name -> dotted OID. Case-insensitive on the
 * name side; exact match on the OID side. */
SELECT asn1_oid_for('commonName');
SELECT asn1_oid_for('COMMONNAME');
SELECT asn1_oid_for('sha256WithRSAEncryption');
SELECT asn1_oid_for('Ed25519');

/* Unknown name -> NULL. */
SELECT asn1_oid_for('nonExistentAlg');

/* ---- DER validity check ----
 * A minimal INTEGER 1: tag=02, len=01, content=01.
 * is_valid_der requires canonical DER (round-trips byte-exact). */
SELECT asn1_is_valid_der(x'020101');

/* A SEQUENCE { INTEGER 1, INTEGER 2 }:
 *   30 06         SEQUENCE, len 6
 *     02 01 01    INTEGER 1
 *     02 01 02    INTEGER 2 */
SELECT asn1_is_valid_der(x'3006020101020102');

/* Random bytes -> 0. */
SELECT asn1_is_valid_der(x'deadbeef');
SELECT asn1_is_valid_der(x'ff');

/* ---- Type tag (first byte) ----
 * SEQUENCE = 0x30 = 48; INTEGER = 0x02 = 2; OCTET STRING = 0x04 = 4;
 * OBJECT IDENTIFIER = 0x06 = 6. */
SELECT asn1_type_tag(x'3006020101020102');
SELECT asn1_type_tag(x'020101');
SELECT asn1_type_tag(x'0403aabbcc');
SELECT asn1_type_tag(x'06092a864886f70d01010b');

/* ---- Decode ----
 * INTEGER 1 -> JSON object with type INTEGER, value "1". */
SELECT json_extract(asn1_decode(x'020101'), '$.type');
SELECT json_extract(asn1_decode(x'020101'), '$.value');

/* INTEGER 42. */
SELECT json_extract(asn1_decode(x'02012a'), '$.value');

/* NULL block. */
SELECT json_extract(asn1_decode(x'0500'), '$.type');

/* BOOLEAN TRUE (DER mandates 0xFF for true). */
SELECT json_extract(asn1_decode(x'010100'), '$.value');
SELECT json_extract(asn1_decode(x'0101ff'), '$.value');

/* SEQUENCE { INTEGER 1, INTEGER 2 } -> children array. */
SELECT json_extract(asn1_decode(x'3006020101020102'), '$.type');
SELECT json_extract(asn1_decode(x'3006020101020102'), '$.children[0].value');
SELECT json_extract(asn1_decode(x'3006020101020102'), '$.children[1].value');

/* OID decode -> dotted string + curated name when known.
 * sha256WithRSAEncryption: 1.2.840.113549.1.1.11
 *   bytes = 06 09 2a 86 48 86 f7 0d 01 01 0b */
SELECT json_extract(asn1_decode(x'06092a864886f70d01010b'), '$.value');
SELECT json_extract(asn1_decode(x'06092a864886f70d01010b'), '$.name');

/* ---- Round-trip: decode -> encode -> bytes match -----
 * asn1_encode(asn1_decode(blob)) == blob for the SEQUENCE {1,2}. */
SELECT hex(asn1_encode(asn1_decode(x'3006020101020102')))
       = upper('3006020101020102');

/* Round-trip a longer SEQUENCE containing an OID. */
SELECT hex(asn1_encode(asn1_decode(x'06092a864886f70d01010b')))
       = upper('06092a864886f70d01010b');

/* ---- Encode from a hand-written JSON tree ----
 * Build SEQUENCE { INTEGER 1, INTEGER 2 } from scratch. */
SELECT hex(asn1_encode(
  '{"type":"SEQUENCE","children":[
     {"type":"INTEGER","value":"1"},
     {"type":"INTEGER","value":"2"}
   ]}'))
  = upper('3006020101020102');

/* Encode an OID; bytes should match the canonical DER above. */
SELECT hex(asn1_encode(
  '{"type":"OBJECT IDENTIFIER","value":"1.2.840.113549.1.1.11"}'))
  = upper('06092a864886f70d01010b');

/* ---- Pretty-print ----
 * Multi-line indented JSON output. Just check that at least one
 * newline is in there (indentation visible). */
SELECT instr(asn1_pretty(x'3006020101020102'), char(10)) > 0;

/* ---- Malformed -> NULL on decode/pretty -----
 * asn1_decode of garbage returns NULL (not an error). */
SELECT asn1_decode(x'ff') IS NULL;
SELECT asn1_pretty(x'ff') IS NULL;

/* NULL in -> NULL out. */
SELECT asn1_decode(NULL) IS NULL;
SELECT asn1_oid_name(NULL) IS NULL;

/* ---- Version is non-empty TEXT. */
SELECT length(asn1_version()) > 0;
