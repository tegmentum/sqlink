.load extensions/natsort/target/wasm32-wasip2/release/natsort_extension.component.wasm

/* The classic natural-sort test: 2 < 10, not 10 < 2. */
SELECT natsort_compare('file2', 'file10');    /* -1 */
SELECT natsort_compare('file10', 'file2');    /* 1 */
SELECT natsort_compare('file2', 'file2');     /* 0 */

/* Compare across mixed structure. */
SELECT natsort_compare('1.0.10', '1.0.2');    /* 1 */
SELECT natsort_compare('1.0.2', '1.0.10');    /* -1 */
SELECT natsort_compare('a1b', 'a10b');        /* -1 */

/* Case insensitive on text segments. */
SELECT natsort_compare('Apple', 'banana');    /* -1 */
SELECT natsort_compare('APPLE', 'apple');     /* 0  case-insensitive */

/* Leading-zero tie-break: equal value, shorter sorts first. */
SELECT natsort_compare('1', '01');            /* -1  same value, 1 has fewer digits */
SELECT natsort_compare('01', '1');            /* 1 */

/* natsort_less convenience. */
SELECT natsort_less('file2', 'file10');       /* 1  true */
SELECT natsort_less('file10', 'file2');       /* 0  false */

/* Key  ORDER BY natsort_key(col) gives natural order under bytewise sort. */
SELECT natsort_compare(natsort_key('file2'), natsort_key('file10')) < 0;  /* 1 */
SELECT natsort_compare(natsort_key('file100'), natsort_key('file20')) > 0; /* 1 */

/* Empty strings tie. */
SELECT natsort_compare('', '');               /* 0 */
SELECT natsort_compare('', 'a');              /* -1 */
