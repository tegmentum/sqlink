.load extensions/google-polyline/target/wasm32-wasip2/release/google_polyline_extension.component.wasm

/* Canonical example from the Google polyline algorithm spec:
 * [[38.5, -120.2], [40.7, -120.95], [43.252, -126.453]]
 * encodes to "_p~iF~ps|U_ulLnnqC_mqNvxq`@". */
SELECT polyline_encode('[[38.5,-120.2],[40.7,-120.95],[43.252,-126.453]]');

/* Decode round-trip. */
SELECT polyline_decode('_p~iF~ps|U_ulLnnqC_mqNvxq`@');

/* Length count. */
SELECT polyline_length('_p~iF~ps|U_ulLnnqC_mqNvxq`@');    /* 3 */
SELECT polyline_length('');                                 /* 0 (empty) */

/* Round-trip property: decode(encode(x))  x (within 1e-5 precision). */
SELECT polyline_decode(polyline_encode('[[40.7128,-74.0060]]'));

/* Single coord. */
SELECT polyline_encode('[[0.0, 0.0]]');                     /* ?? */
SELECT polyline_decode('??');                                /* [[0,0]] */

/* Fail-clean cases. */
SELECT polyline_encode('not json');
SELECT polyline_encode('[1, 2, 3]');                        /* shape wrong */
SELECT polyline_decode('invalid bytes below 63');          /* NULL */
