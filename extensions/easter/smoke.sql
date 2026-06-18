.load extensions/easter/target/wasm32-wasip2/release/easter_extension.component.wasm

/* Western Easter dates  classic reference cases verified
 * against the Catholic Church's published table. */
SELECT easter_western(2025);     /* 2025-04-20 */
SELECT easter_western(2024);     /* 2024-03-31 */
SELECT easter_western(2000);     /* 2000-04-23 */
SELECT easter_western(1583);     /* 1583-04-10  earliest valid */

/* Orthodox Easter  often a week later, sometimes 5 weeks. */
SELECT easter_orthodox(2025);    /* 2025-04-20 (rare alignment) */
SELECT easter_orthodox(2024);    /* 2024-05-05 (5 weeks after Western) */
SELECT easter_orthodox(2023);    /* 2023-04-16 (1 week after Western) */

/* Easter offsets: derived holidays for Western 2025.
 * Ash Wed = Easter-46 (Mar 5); -47 included as an arithmetic
 * cross-check. Other reference dates match liturgical convention. */
SELECT easter_offset(2025, -47, 'western');   /* 2025-03-04 */
SELECT easter_offset(2025, -2, 'western');    /* Good Friday: 2025-04-18 */
SELECT easter_offset(2025, 1, 'western');     /* Easter Monday: 2025-04-21 */
SELECT easter_offset(2025, 39, 'western');    /* Ascension: 2025-05-29 */
SELECT easter_offset(2025, 49, 'western');    /* Pentecost: 2025-06-08 */

/* Offset with orthodox calendar. */
SELECT easter_offset(2024, -2, 'orthodox');   /* Orthodox Good Friday 2024-05-03 */

/* Fail-clean: pre-1583  Julian calendar transition  NULL. */
SELECT easter_western(1582);
SELECT easter_orthodox(1582);

/* Western works for any year >= 1583 (no Julian shift needed). */
SELECT easter_western(2300);     /* 2300-04-08 */

/* Orthodox needs the Julian->Gregorian shift table; beyond 2199  NULL. */
SELECT easter_orthodox(2300);
