.load extensions/humansize/target/wasm32-wasip2/release/humansize_extension.component.wasm

/* Decimal byte formatter: 1500  "1.5 KB". */
SELECT humansize_bytes(0);          /* "0 B" */
SELECT humansize_bytes(999);
SELECT humansize_bytes(1500);
SELECT humansize_bytes(1500000);
SELECT humansize_bytes(2.5 * 1e9);

/* Binary byte formatter: 1024  "1 KiB". */
SELECT humansize_ibytes(1024);
SELECT humansize_ibytes(1536);      /* "1.5 KiB" */
SELECT humansize_ibytes(1048576);

/* Round-trip: parse the formatter's own output. */
SELECT humansize_parse_bytes('1.5 KB');     /* 1500 */
SELECT humansize_parse_bytes('1.5KiB');     /* 1536 */
SELECT humansize_parse_bytes('2 GB');
SELECT humansize_parse_bytes('100B');
SELECT humansize_parse_bytes('500 bytes');
SELECT humansize_parse_bytes('not a size'); /* NULL */

/* Duration formatter: 1-2 most-significant units. */
SELECT humansize_duration(0);       /* "0s" */
SELECT humansize_duration(45);      /* "45s" */
SELECT humansize_duration(90);      /* "1m 30s" */
SELECT humansize_duration(3700);    /* "1h 1m" */
SELECT humansize_duration(86400);   /* "1d" */
SELECT humansize_duration(90061);   /* "1d 1h" */

/* Duration parser. */
SELECT humansize_parse_duration('90s');     /* 90 */
SELECT humansize_parse_duration('1h 30m');  /* 5400 */
SELECT humansize_parse_duration('2d');      /* 172800 */
SELECT humansize_parse_duration('garbage'); /* NULL */
