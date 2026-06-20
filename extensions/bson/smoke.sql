.load extensions/bson/target/wasm32-wasip2/release/bson_extension.component.wasm

/* ─── Byte-exact acceptance vector ─── */

/* Empty BSON document is 5 bytes: 0x05 0x00 0x00 0x00 0x00.
   (4 bytes LE length prefix + the 0x00 EOO terminator.) */
SELECT hex(bson_encode('{}'));

/* ─── is_valid ─── */
SELECT bson_is_valid(bson_encode('{}'));
SELECT bson_is_valid(X'05000000ff');           -- length OK but no EOO
SELECT bson_is_valid(X'ffffff');               -- garbage

/* ─── Round-trip ───
   bson_decode(bson_encode(json)) parses back to the same JSON.
   bson 2.x's Extended JSON emits ints as raw integers in Relaxed
   mode, so `{"a":1,"b":[2,3]}` round-trips byte-for-byte. */
SELECT bson_decode(bson_encode('{"a":1,"b":[2,3]}'));

/* String round-trip. */
SELECT bson_decode(bson_encode('{"k":"hello"}'));

/* Nested object round-trip. */
SELECT bson_decode(bson_encode('{"x":{"y":{"z":42}}}'));

/* ─── extract ─── */

/* Simple nested key. */
SELECT bson_extract(bson_encode('{"a":{"b":{"c":42}}}'), 'a.b.c');

/* Array index. */
SELECT bson_extract(bson_encode('{"xs":[10,20,30]}'), 'xs.1');

/* Missing path  NULL. */
SELECT bson_extract(bson_encode('{"a":1}'), 'b');

/* Top-level key. */
SELECT bson_extract(bson_encode('{"name":"alice"}'), 'name');

/* ─── ObjectId ─── */

/* New ObjectId is 24 lowercase hex chars. */
SELECT length(bson_object_id());
SELECT bson_object_id() GLOB '[0-9a-f]*';

/* Embedded timestamp round-trips through the helper.
   We can't assert the absolute value (depends on test wall clock)
   but we can assert structure: the recovered ms is > 0 and within
   reasonable bounds (> year 2020 = 1577836800000 ms).
   `~~` would also work; assert >= 0 keeps the smoke deterministic. */
SELECT bson_object_id_to_ts(bson_object_id()) >= 1577836800000;

/* Known fixed ObjectId  ts test. The leading 4 bytes are seconds
   since epoch; '5e0000000000000000000000'  0x5e000000
   = 1577058304 seconds  1577058304000 ms. */
SELECT bson_object_id_to_ts('5e0000000000000000000000');

/* Malformed ObjectId  NULL. */
SELECT bson_object_id_to_ts('not-an-oid');
SELECT bson_object_id_to_ts('5e000000');   -- too short

/* ─── Decode malformed  NULL ─── */
SELECT bson_decode(X'ffffff');

/* ─── Top-level non-object JSON  NULL on encode ─── */
SELECT bson_encode('[1,2,3]');
SELECT bson_encode('42');

/* ─── NULL in  NULL out ─── */
SELECT bson_encode(NULL);
SELECT bson_decode(NULL);
SELECT bson_extract(NULL, 'a');
SELECT bson_object_id_to_ts(NULL);
/* is_valid on NULL  0 (NULL isn't a valid BSON document). */
SELECT bson_is_valid(NULL);

/* ─── Version non-empty ─── */
SELECT length(bson_version()) > 0;
