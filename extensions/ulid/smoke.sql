.load extensions/ulid/target/wasm32-wasip2/release/ulid_extension.component.wasm

/* Default ulid() returns 26 chars of Crockford base32. */
SELECT length(ulid());

/* ulid_blob() returns 16 raw big-endian bytes. */
SELECT length(ulid_blob());

/* Round-trip a known-timestamp ULID through ulid_timestamp(). */
SELECT ulid_timestamp(ulid_from(1700000000000));

/* ulid_from(0) yields a ULID whose timestamp portion decodes to 0. */
SELECT ulid_timestamp(ulid_from(0));

/* Time ordering: ulid_from(small) < ulid_from(large) lexicographically. */
SELECT ulid_from(1000000000000) < ulid_from(2000000000000);

/* ulid_random_part returns 10 bytes (80 bits). */
SELECT length(ulid_random_part(ulid_from(1700000000000)));

/* Parse-failure path: invalid ulid string => NULL. */
SELECT ulid_timestamp('not-a-ulid');
SELECT ulid_random_part('not-a-ulid');

/* Known-vector parse: 01ARZ3NDEKTSV4RRFFQ69G5FAV is the canonical
 * example ULID from the spec README; first 10 chars are the 48-bit
 * timestamp half. Crockford base32 of "01ARZ3NDEK" decodes to ms
 * epoch 1469922850259  matches what ulid-rs's Ulid::from_string
 * + timestamp_ms() return for this value. */
SELECT ulid_timestamp('01ARZ3NDEKTSV4RRFFQ69G5FAV');

/* Crockford base32: a ULID has no lowercase letters and contains
 * only allowed chars (no I, L, O, U). Verify against a CTE that
 * stages one ULID so we don't recompute mid-query. */
WITH t(u) AS (VALUES (ulid()))
SELECT (upper(u) = u) AND
       (u GLOB '[0-9A-Z]*') AND
       (u NOT GLOB '*[ILOU]*')
FROM t;
