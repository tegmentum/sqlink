.load extensions/color/target/wasm32-wasip2/release/color_extension.component.wasm

/* Mixed input forms  hex (short/long, with/without #), rgb()
 * function syntax, named CSS basic-16 colors. All round-trip to
 * canonical lowercase #rrggbb. */
SELECT color_to_hex('#ff8800');
SELECT color_to_hex('FFFFFF');
SELECT color_to_hex('#abc');         /* short hex expands #aabbcc */
SELECT color_to_hex('rgb(0, 128, 255)');
SELECT color_to_hex('rgba(10, 20, 30, 0.5)');
SELECT color_to_hex('Aqua');         /* named */
SELECT color_to_hex('not-a-color');  /* NULL on failure */

SELECT color_to_rgb('#ff8800');

/* Channel extractors. */
SELECT color_red('#ff8800');
SELECT color_green('#ff8800');
SELECT color_blue('#ff8800');

/* WCAG luminance: white=1.0, black=0.0. Round to 4 dp so the
 * FP representation is stable. */
SELECT round(color_luminance('#ffffff'), 4);
SELECT round(color_luminance('#000000'), 4);

/* WCAG contrast ratio: black-on-white=21.0 (max), white-on-white=1.0
 * (min). Order-independent  symmetry verified separately. */
SELECT round(color_contrast_ratio('#000000', '#ffffff'), 2);
SELECT round(color_contrast_ratio('#ffffff', '#000000'), 2);
SELECT round(color_contrast_ratio('#777777', '#ffffff'), 2);
