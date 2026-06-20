.load extensions/binary-codecs/target/wasm32-wasip2/release/binary_codecs_extension.component.wasm

/* ─── Byte-exact acceptance vectors ─── */

/* CBOR integer 1 → 0x01 (CBOR major type 0, value 1). */
SELECT hex(cbor_encode(1));

/* CBOR text "hi" → 0x62 0x68 0x69 (text-string, len 2, 'h','i'). */
SELECT hex(cbor_encode('hi'));

/* MessagePack empty map → 0x80 (fixmap with len 0). */
SELECT hex(msgpack_encode('{}'));

/* MessagePack integer 1 (sanity check, positive fixint). */
SELECT hex(msgpack_encode(1));

/* ─── CBOR round-trip ───
 * cbor_decode(cbor_encode(json('{"a":1,"b":[2,3]}'))) should parse
 * back to the same JSON. We use json() to normalize key order on
 * both sides (SQLite's json_object orders deterministically). */
SELECT cbor_decode(cbor_encode('{"a":1,"b":[2,3]}'));

/* ─── MessagePack round-trip ─── */
SELECT msgpack_decode(msgpack_encode('{"a":1,"b":[2,3]}'));

/* ─── Primitives round-trip ───
 * SQL primitives pass through cleanly: INTEGER, REAL, TEXT, BLOB. */
SELECT cbor_decode(cbor_encode(42));
SELECT cbor_decode(cbor_encode('hello'));
SELECT msgpack_decode(msgpack_encode(42));
SELECT msgpack_decode(msgpack_encode('hello'));

/* ─── Boolean / null round-trip via JSON shape ─── */
SELECT cbor_decode(cbor_encode('true'));
SELECT cbor_decode(cbor_encode('null'));
SELECT msgpack_decode(msgpack_encode('false'));

/* ─── NULL in → NULL out ─── */
SELECT cbor_encode(NULL);
SELECT cbor_decode(NULL);
SELECT msgpack_encode(NULL);
SELECT msgpack_decode(NULL);

/* ─── Malformed blob → NULL (documented behavior) ─── */
SELECT cbor_decode(X'ffffff');
SELECT msgpack_decode(X'c1');

/* ─── Array shape round-trip ─── */
SELECT cbor_decode(cbor_encode('[1,2,3]'));
SELECT msgpack_decode(msgpack_encode('[1,2,3]'));

/* ─── Version is non-empty ─── */
SELECT length(binary_codecs_version()) > 0;
