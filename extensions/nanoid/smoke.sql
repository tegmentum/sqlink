.load extensions/nanoid/target/wasm32-wasip2/release/nanoid_extension.component.wasm

/* nanoid() is nondet  pin the format (length + alphabet), not bytes. */

/* Default: 21 chars from the URL-safe alphabet [A-Za-z0-9_-]. */
SELECT length(nanoid()) = 21;
SELECT nanoid() GLOB '[A-Za-z0-9_-]*';

/* Custom length via nanoid_n. */
SELECT length(nanoid_n(8)) = 8;
SELECT length(nanoid_n(32)) = 32;
SELECT nanoid_n(64) GLOB '[A-Za-z0-9_-]*';

/* Length clamp: huge requests get capped to MAX_LEN=256. */
SELECT length(nanoid_n(100000)) = 256;

/* Custom alphabet only emits the requested chars. */
SELECT nanoid_alpha(8, 'abc') GLOB '[abc]*';
SELECT length(nanoid_alpha(16, 'xyz')) = 16;

/* Single-char alphabet is fully deterministic in output. */
SELECT nanoid_alpha(5, 'a');

/* Empty alphabet  error surfaced as NULL by the cli (Err path). */
SELECT nanoid_alpha(8, '');

/* No-collision check at small scale: 1000 default nanoids have
 * 1000 distinct values (probability of collision with 126-bit
 * entropy is < 1 in 2^60). */
WITH RECURSIVE n(i, id) AS (
    SELECT 1, nanoid()
    UNION ALL SELECT i+1, nanoid() FROM n WHERE i < 1000
)
SELECT COUNT(DISTINCT id) FROM n;
