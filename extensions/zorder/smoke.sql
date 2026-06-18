.load extensions/zorder/target/wasm32-wasip2/release/zorder_extension.component.wasm

/* 2D Z-order: small grid by hand.
 * zorder(x=0, y=0) = bits ... = 0
 * zorder(x=1, y=0) = bit0 set  1
 * zorder(x=0, y=1) = bit1 set  2
 * zorder(x=1, y=1) = bits 0+1  3
 * zorder(x=2, y=0) = bit2 set  4
 * zorder(x=3, y=3) = bits 0,1,2,3  15 */
SELECT zorder(0, 0);
SELECT zorder(1, 0);
SELECT zorder(0, 1);
SELECT zorder(1, 1);
SELECT zorder(2, 0);
SELECT zorder(3, 3);

/* 3D and higher arities are supported (zorder.c surface). */
SELECT zorder(1, 1, 1);
SELECT zorder(1, 2, 3, 4);

/* Inverse: extract dimension i from z. */
SELECT unzorder(zorder(5, 7), 2, 0);   /* 5 */
SELECT unzorder(zorder(5, 7), 2, 1);   /* 7 */
SELECT unzorder(zorder(1, 2, 3), 3, 2); /* 3 */

/* Round-trip for non-trivial values. */
SELECT unzorder(zorder(42, 17), 2, 0); /* 42 */
SELECT unzorder(zorder(42, 17), 2, 1); /* 17 */

/* Out-of-range dimension index  NULL. */
SELECT unzorder(0, 2, 5);
