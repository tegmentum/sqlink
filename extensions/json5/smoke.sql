.load extensions/json5/target/wasm32-wasip2/release/json5_extension.component.wasm

/* ─── Validity probes ─── */
SELECT json5_is_valid('{a:1,}');
SELECT json5_is_valid('// hi
{ x: 1 }');
SELECT json5_is_valid(NULL);
SELECT json5_is_valid('');
SELECT json5_is_valid('{not valid');

/* ─── Relaxed JSON5 -> strict JSON round-trip ─── */
SELECT json5_parse('{a:1,b:2,}');
SELECT json5_parse('{ foo: "bar" }');
SELECT json5_parse('{ foo: ''bar'' }');
SELECT json5_parse('// comment
{x:1}');
SELECT json5_parse('{ n: 0xff }');
SELECT json5_parse('[1, 2, 3,]');

/* ─── Bare scalars round-trip ─── */
SELECT json5_parse('42');
SELECT json5_parse('"hi"');
SELECT json5_parse('true');
SELECT json5_parse('null');

/* ─── NULL in -> NULL out for json5_parse ─── */
SELECT json5_parse(NULL);

/* ─── Parsed output is parseable strict JSON (json1 round-trip) ─── */
SELECT json_extract(json5_parse('{ a: 1, b: { c: [10, 20,] } }'), '$.b.c[1]');

/* ─── version non-empty ─── */
SELECT length(json5_version()) > 0;
