.load extensions/beaufort-scale/target/wasm32-wasip2/release/beaufort_scale_extension.component.wasm

/* m/s  force band. */
SELECT beaufort_force(0);             /* 0 (calm) */
SELECT beaufort_force(0.5);           /* 1 (lower boundary of light air) */
SELECT beaufort_force(1.5);           /* 1 (just under 1.6) */
SELECT beaufort_force(1.6);           /* 2 (lower boundary of light breeze) */
SELECT beaufort_force(10.7);          /* 5 (just under 10.8) */
SELECT beaufort_force(10.8);          /* 6 (boundary) */
SELECT beaufort_force(33);            /* 12 (hurricane) */
SELECT beaufort_force(100);           /* 12 (still hurricane  open-ended) */

/* Named force. */
SELECT beaufort_name(0);              /* "Calm" */
SELECT beaufort_name(8);              /* "Fresh breeze" */
SELECT beaufort_name(20);             /* "Gale" (force 8) */
SELECT beaufort_name(35);             /* "Hurricane" */

/* km/h convenience. 36 km/h = 10 m/s  force 5. */
SELECT beaufort_from_kmh(36);         /* 5 */
SELECT beaufort_from_kmh(120);        /* 12 (33.3 m/s  hurricane) */

/* mph convenience. 22 mph = 9.835 m/s  force 5. */
SELECT beaufort_from_mph(22);

/* Reverse: force  lower-bound m/s. */
SELECT beaufort_min_ms(0);            /* 0 */
SELECT beaufort_min_ms(5);            /* 8 */
SELECT beaufort_min_ms(12);           /* 32.7 */
SELECT beaufort_min_ms(13);           /* NULL  out of range */

/* Negative input clamps to force 0. */
SELECT beaufort_force(-5);            /* 0 */
