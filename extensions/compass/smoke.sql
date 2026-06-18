.load extensions/compass/target/wasm32-wasip2/release/compass_extension.component.wasm

/* 8-point cardinals. Bands are centered: 0=N covers [-22.5, 22.5). */
SELECT compass_cardinal(0);          /* N */
SELECT compass_cardinal(45);          /* NE */
SELECT compass_cardinal(90);          /* E */
SELECT compass_cardinal(180);         /* S */
SELECT compass_cardinal(270);         /* W */
SELECT compass_cardinal(360);         /* N (wraps) */

/* Boundary cases  band edges. */
SELECT compass_cardinal(22);          /* N (just under 22.5) */
SELECT compass_cardinal(23);          /* NE (just over) */

/* 16-point cardinals. */
SELECT compass_cardinal16(0);          /* N */
SELECT compass_cardinal16(22.5);       /* NNE */
SELECT compass_cardinal16(45);          /* NE */
SELECT compass_cardinal16(112.5);       /* ESE */

/* Reverse: name  center degrees. */
SELECT compass_degrees('N');          /* 0 */
SELECT compass_degrees('NE');         /* 45 */
SELECT compass_degrees('NNW');        /* 337.5 */
SELECT compass_degrees('nnw');        /* same  case-insensitive */
SELECT compass_degrees('X');          /* NULL */

/* Angular distance: shortest path around the circle. */
SELECT compass_distance(0, 10);       /* 10 */
SELECT compass_distance(0, 350);       /* 10 (wraps, not 350) */
SELECT compass_distance(45, 225);      /* 180 (opposite) */
SELECT compass_distance(370, -10);     /* 20  normalizes both */

/* Normalize to [0, 360). */
SELECT compass_normalize(361);        /* 1 */
SELECT compass_normalize(-1);         /* 359 */
SELECT compass_normalize(720);        /* 0 */
