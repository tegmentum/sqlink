.load extensions/radix/target/wasm32-wasip2/release/radix_extension.component.wasm

/* Common bases: binary, octal, hex. */
SELECT radix_to(255, 2);             /* "11111111" */
SELECT radix_to(255, 16);            /* "FF" */
SELECT radix_to(255, 8);             /* "377" */
SELECT radix_to(0, 16);              /* "0" */
SELECT radix_to(-42, 16);            /* "-2A" */

/* Higher bases  letters from A-Z. */
SELECT radix_to(35, 36);             /* "Z" */
SELECT radix_to(1295, 36);           /* "ZZ"  35*36 + 35 */

/* Parse. Lowercase + uppercase accepted. */
SELECT radix_from('FF', 16);         /* 255 */
SELECT radix_from('ff', 16);         /* same */
SELECT radix_from('11111111', 2);    /* 255 */
SELECT radix_from('-2A', 16);
SELECT radix_from('not hex', 16);    /* NULL */

/* Direct base-to-base. */
SELECT radix_change('FF', 16, 2);    /* "11111111" */
SELECT radix_change('11111111', 2, 16); /* "FF" */

/* Digit count + bit count. */
SELECT radix_digits(255, 10);        /* 3 */
SELECT radix_digits(255, 2);         /* 8 */
SELECT radix_digits(0, 10);          /* 1 */
SELECT radix_bits(255);              /* 8 */
SELECT radix_bits(256);              /* 9 */
SELECT radix_bits(0);                /* 1 */

/* Fail-clean: out-of-range base. */
SELECT radix_to(100, 1);
SELECT radix_to(100, 37);
