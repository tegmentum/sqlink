.load extensions/uuid/target/wasm32-wasip2/release/uuid_extension.component.wasm

/* uuid generators (nondet) just verify the format looks UUID-y. */
SELECT length(uuid()) = 36;
SELECT length(uuidv4()) = 36;
SELECT length(uuidv7()) = 36;

/* nil + parse + version + variant */
SELECT uuid_nil();
SELECT uuid_validate('550e8400-e29b-41d4-a716-446655440000');
SELECT uuid_validate('not-a-uuid');
SELECT uuid_version('550e8400-e29b-41d4-a716-446655440000');
SELECT uuid_variant('550e8400-e29b-41d4-a716-446655440000');
SELECT uuid_version(uuidv4());
SELECT uuid_version(uuidv7());

/* timestamp extraction: v7 UUID has ms since epoch in the top 48 bits */
SELECT uuid_timestamp_ms(uuidv7()) > 1700000000000;
SELECT uuid_timestamp_ms('550e8400-e29b-41d4-a716-446655440000');

/* PLAN #5 v7 surface: uuid_v7 (alias), uuid_v7_blob (16-byte), uuid_v7_timestamp */
SELECT length(uuid_v7()) = 36;
SELECT uuid_version(uuid_v7());
SELECT length(uuid_v7_blob());                                       -- 16 bytes
SELECT uuid_v7_timestamp(uuid_v7()) > 1700000000000;
SELECT uuid_v7_timestamp(uuid_v7_blob()) > 1700000000000;            -- BLOB path
SELECT uuid_v7_timestamp('550e8400-e29b-41d4-a716-446655440000');    -- v4: NULL
