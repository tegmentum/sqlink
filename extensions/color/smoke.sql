.load extensions/color/target/wasm32-wasip2/release/color_extension.component.wasm

/* ---- v0.1 surface (kept for back-compat) ---- */

/* Mixed input forms - hex (short/long, with/without #), rgb()
 * function syntax, named CSS colors. All round-trip to canonical
 * lowercase #rrggbb. */
SELECT color_to_hex('#ff8800');
SELECT color_to_hex('FFFFFF');
SELECT color_to_hex('#abc');         /* short hex expands #aabbcc */
SELECT color_to_hex('rgb(0, 128, 255)');
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

/* WCAG contrast ratio: black-on-white = 21.0 (max), white-on-white = 1.0
 * (min). Order-independent. */
SELECT round(color_contrast_ratio('#000000', '#ffffff'), 2);
SELECT round(color_contrast_ratio('#ffffff', '#000000'), 2);
SELECT round(color_contrast_ratio('#777777', '#ffffff'), 2);

/* ---- v0.2 surface (PLAN-more-extensions-3 #5) ---- */

/* color_parse: accepts any CSS-recognized form, returns canonical
 * lowercase '#rrggbb'. */
SELECT color_parse('red');
SELECT color_parse('#F00');
SELECT color_parse('rgb(255, 0, 0)');
SELECT color_parse('hsl(0, 100%, 50%)');
SELECT color_parse('rebeccapurple');
SELECT color_parse('not-a-color');         /* NULL */

/* color_named: lookup-only, returns NULL for non-name inputs. */
SELECT color_named('rebeccapurple');
SELECT color_named('papayawhip');
SELECT color_named('REBECCAPURPLE');       /* case-insensitive */
SELECT color_named('#ff0000');             /* NULL: not a name */
SELECT color_named('notacolor');           /* NULL: unknown name */

/* color_rgb_to_hex */
SELECT color_rgb_to_hex(255, 0, 0);
SELECT color_rgb_to_hex(0, 128, 255);

/* color_hex_to_rgb */
SELECT color_hex_to_rgb('#ff0000');
SELECT color_hex_to_rgb('#0080ff');

/* color_rgb_to_hsl: pure red -> '[0, 100, 50]'. The hue for pure
 * grey is reported as 0 (NaN-clamped). */
SELECT color_rgb_to_hsl(255, 0, 0);
SELECT color_rgb_to_hsl(0, 255, 0);        /* green: h=120 */
SELECT color_rgb_to_hsl(128, 128, 128);    /* grey: h=0, s=0, l~50 */

/* color_hsl_to_rgb: round-trip. h 0..360, s/l 0..100 (percent). */
SELECT color_hsl_to_rgb(0, 100, 50);       /* red */
SELECT color_hsl_to_rgb(120, 100, 50);     /* green */
SELECT color_hsl_to_rgb(240, 100, 50);     /* blue */

/* color_rgb_to_hsv */
SELECT color_rgb_to_hsv(255, 0, 0);
SELECT color_rgb_to_hsv(0, 0, 0);

/* color_hsv_to_rgb */
SELECT color_hsv_to_rgb(0, 100, 100);      /* red */
SELECT color_hsv_to_rgb(120, 100, 100);    /* green */

/* color_invert */
SELECT color_invert('#000000');
SELECT color_invert('#ffffff');
SELECT color_invert('red');                /* 255-r, 255-g, 255-b -> #00ffff */

/* color_mix: linear-light interpolation (gamma-correct). The
 * endpoints round-trip exactly; the midpoint of '#000000' and
 * '#ffffff' is *not* '#808080' because the mean of linear-light
 * intensities (0.5) re-encodes to sRGB ~ 0.7253 -> 185 -> '#b9b9b9'.
 * This is the gamma-correct answer color tools want; the sRGB-space
 * midpoint can be derived via color_rgb_to_hex(128,128,128). */
SELECT color_mix('#000000', '#ffffff', 0.0);
SELECT color_mix('#000000', '#ffffff', 1.0);
SELECT color_mix('#000000', '#ffffff', 0.5);
SELECT color_mix('#ff0000', '#0000ff', 0.5);  /* midpoint of red+blue in linear-light */

/* color_version: cargo pkg version + parser crate name. */
SELECT color_version();

/* NULL propagation */
SELECT color_parse(NULL);
SELECT color_named(NULL);
SELECT color_invert(NULL);
SELECT color_mix(NULL, '#ffffff', 0.5);
