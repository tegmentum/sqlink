.load extensions/tile/target/wasm32-wasip2/release/tile_extension.component.wasm

/* lat/lon  tile xyz. NYC (40.7128, -74.0060) at zoom 10:
 * x=301, y=385. */
SELECT tile_x(-74.0060, 10);          /* 301 */
SELECT tile_y(40.7128, 10);           /* 385 */

/* (0, 0) at zoom 0  exactly one tile. */
SELECT tile_x(0.0, 0);
SELECT tile_y(0.0, 0);

/* Round-trip through tile_lon/tile_lat: tile center recovery. */
SELECT round(tile_lon(301, 10), 4);   /* -74.0625 (NW corner of tile) */
SELECT round(tile_lat(385, 10), 4);   /*  40.7799 (NW corner of tile) */

/* Quadkey: (3, 5, 3) =
 *   z=3, x bits 011, y bits 101  digits 0/1/1+2,1/0/1+2  213. */
SELECT tile_quadkey(3, 5, 3);          /* "213" */
/* Zoom 0 quadkey is empty by spec  whole world is one tile.
 * Empty rows are stripped by parse_results, so wrap with a sentinel. */
SELECT coalesce(nullif(tile_quadkey(0, 0, 0), ''), '<empty>');
SELECT tile_quadkey(0, 0, 5);          /* "00000" */

/* Round-trip quadkey: encode then decode  same (x, y, z). */
SELECT tile_from_quadkey('213');
SELECT tile_from_quadkey('00000');
SELECT tile_from_quadkey('not a quadkey');  /* NULL */
SELECT tile_from_quadkey('');               /* NULL  zoom 0 invalid */

/* Bounding box  JSON {west, south, east, north}. */
SELECT tile_bbox(0, 0, 0);            /* whole world */

/* Web Mercator clamp: latitudes beyond 85.05 clamp to the pole tile. */
SELECT tile_y(89.0, 5);                /* clamped to 0 */
SELECT tile_y(-89.0, 5);               /* clamped to 31 */
