.load extensions/toml/target/wasm32-wasip2/release/toml_extension.component.wasm

/* ─── Validity ─── */
SELECT toml_is_valid('a = 1');
SELECT toml_is_valid('not toml [[[ ');
SELECT toml_is_valid(NULL);

/* ─── toml_to_json basic ─── */
SELECT toml_to_json('[server]
port = 8080
host = "localhost"
');

/* ─── toml_get dotted ─── */
SELECT toml_get('[server]
port = 8080
host = "localhost"
', 'server.port');

SELECT toml_get('[server]
port = 8080
host = "localhost"
', 'server.host');

SELECT toml_get('[server]
port = 8080
host = "localhost"
', 'server.missing');

/* ─── toml_keys (root + nested) ─── */
SELECT toml_keys('[server]
port = 8080
host = "localhost"
');

SELECT toml_keys('[server]
port = 8080
host = "localhost"
', 'server');

/* ─── Round-trip through JSON ─── */
SELECT toml_is_valid(json_to_toml(toml_to_json('[server]
port = 8080
host = "localhost"
')));

/* ─── json_to_toml on a bare scalar (wrapped) ─── */
SELECT toml_is_valid(json_to_toml('42'));

/* ─── NULL in → NULL out ─── */
SELECT toml_to_json(NULL);
SELECT json_to_toml(NULL);
SELECT toml_get(NULL, 'a');
SELECT toml_keys(NULL);

/* ─── version non-empty ─── */
SELECT length(toml_version()) > 0;
