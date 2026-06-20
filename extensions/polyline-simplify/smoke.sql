.load extensions/polyline-simplify/target/wasm32-wasip2/release/polyline_simplify_extension.component.wasm

/* ── Douglas-Peucker happy path ───────────────────────────────
 * Five collinear points along y=x. With any positive tolerance
 * the three middle points are dropped and only the endpoints
 * survive. */
SELECT polyline_simplify_dp('[[0,0],[1,1],[2,2],[3,3],[4,4]]', 0.001);

/* Geo crate's own doc example: a polyline with real corners.
 * At tolerance 1.0 the (17.3, 3.2) point is dropped per the
 * upstream doctest. */
SELECT polyline_simplify_dp(
  '[[0,0],[5,4],[11,5.5],[17.3,3.2],[27.8,0.1]]',
  1.0);

/* ── Visvalingam-Whyatt happy path ────────────────────────────
 * Geo crate's own VW doc example -- with tolerance 30 the
 * (3, 8) and (6, 20) points should drop, leaving the first +
 * sharpest peak + last. */
SELECT polyline_simplify_vw(
  '[[5,2],[3,8],[6,20],[7,25],[10,10]]',
  30.0);

/* ── First + last always preserved (length stays >= 2 for any
 * input with >= 2 points). Aggressive tolerance can't collapse
 * a polyline to a single point. */
SELECT json_array_length(
  polyline_simplify_dp('[[0,0],[1,1],[2,2],[3,3],[4,4]]', 1000.0));

/* ── Two-point passthrough ─────────────────────────────────── */
SELECT polyline_simplify_dp('[[0,0],[1,1]]', 1.0);
SELECT polyline_simplify_vw('[[0,0],[1,1]]', 1.0);

/* ── Empty array round-trips ──────────────────────────────── */
SELECT polyline_simplify_dp('[]', 1.0);

/* ── NULL pass-through ────────────────────────────────────── */
SELECT polyline_simplify_dp(NULL, 1.0);
SELECT polyline_simplify_vw('[[0,0],[1,1]]', NULL);

/* ── Bad JSON → NULL (not an error) ───────────────────────── */
SELECT polyline_simplify_dp('not json', 1.0);
SELECT polyline_simplify_vw('{"x":1}', 1.0);
SELECT polyline_simplify_dp('[1,2,3]', 1.0);
SELECT polyline_simplify_dp('[[1]]', 1.0);
SELECT polyline_simplify_vw('[["a","b"]]', 1.0);

/* ── Version is non-empty ─────────────────────────────────── */
SELECT length(polyline_simplify_version()) > 0;
