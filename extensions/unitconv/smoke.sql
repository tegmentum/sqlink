.load extensions/unitconv/target/wasm32-wasip2/release/unitconv_extension.component.wasm

/* Lengths: 1 mile  meters (exact: 1609.344), and back. */
SELECT round(conv_length(1, 'mi', 'm'), 3);
SELECT round(conv_length(1609.344, 'm', 'mi'), 6);
SELECT round(conv_length(100, 'cm', 'in'), 4);     /* 39.3701 */

/* Mass: 1 lb  grams (exact: 453.59237). */
SELECT round(conv_mass(1, 'lb', 'g'), 5);
SELECT round(conv_mass(1, 'kg', 'lb'), 5);         /* 2.20462 */

/* Time. */
SELECT round(conv_time(1, 'h', 's'), 0);           /* 3600 */
SELECT round(conv_time(86400, 's', 'd'), 0);       /* 1 */

/* Temperature  the affine path. */
SELECT round(conv_temperature(0, 'C', 'F'), 1);    /* 32.0 */
SELECT round(conv_temperature(100, 'C', 'F'), 1);  /* 212.0 */
SELECT round(conv_temperature(98.6, 'F', 'C'), 1); /* 37.0 */
SELECT round(conv_temperature(0, 'C', 'K'), 2);    /* 273.15 */
SELECT round(conv_temperature(-40, 'C', 'F'), 1);  /* -40.0 (the fixed point) */

/* Data: decimal vs binary prefixes are different. */
SELECT round(conv_data(1, 'KiB', 'b'), 0);         /* 1024 */
SELECT round(conv_data(1, 'KB', 'b'), 0);          /* 1000 */
SELECT round(conv_data(1, 'GiB', 'MiB'), 0);       /* 1024 */

/* Fail-clean: unknown unit  NULL. */
SELECT conv_length(1, 'm', 'parsec');
SELECT conv_temperature(0, 'C', 'foo');
