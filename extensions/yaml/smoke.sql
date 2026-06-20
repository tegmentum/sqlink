.load extensions/yaml/target/wasm32-wasip2/release/yaml_extension.component.wasm

/* --- Validity --- */
SELECT yaml_is_valid('a: 1');
SELECT yaml_is_valid('{this: is, : broken');
SELECT yaml_is_valid(NULL);

/* --- yaml_to_json: simple mapping --- */
SELECT yaml_to_json('name: Alice
age: 30
');

/* --- yaml_to_json: nested mapping with sequence --- */
SELECT yaml_to_json('server:
  port: 8080
  hosts:
    - a
    - b
');

/* --- yaml_get: scalar lookup --- */
SELECT yaml_get('name: Alice
age: 30
', 'name');
SELECT yaml_get('name: Alice
age: 30
', 'age');

/* --- yaml_get: nested + sequence index --- */
SELECT yaml_get('server:
  port: 8080
  hosts:
    - a
    - b
', 'server.port');
SELECT yaml_get('server:
  port: 8080
  hosts:
    - a
    - b
', 'server.hosts.1');

/* --- yaml_get: missing key -> NULL --- */
SELECT yaml_get('a: 1', 'missing');

/* --- yaml_keys: root + nested --- */
SELECT yaml_keys('server:
  port: 8080
  hosts:
    - a
    - b
');
SELECT yaml_keys('server:
  port: 8080
  hosts:
    - a
    - b
', 'server');

/* --- yaml_keys on a sequence -> NULL --- */
SELECT yaml_keys('server:
  port: 8080
  hosts:
    - a
    - b
', 'server.hosts');

/* --- Round-trip through YAML stays valid YAML --- */
SELECT yaml_is_valid(json_to_yaml(yaml_to_json('a: 1
b: 2
')));

/* --- json_to_yaml on a bare scalar still parses as YAML --- */
SELECT yaml_is_valid(json_to_yaml('42'));

/* --- json1 round-trip on yaml_to_json output --- */
SELECT json_extract(yaml_to_json('outer:
  inner:
    - 10
    - 20
'), '$.outer.inner[1]');

/* --- NULL in -> NULL out (validity uses 0) --- */
SELECT yaml_to_json(NULL);
SELECT json_to_yaml(NULL);
SELECT yaml_get(NULL, 'a');
SELECT yaml_keys(NULL);

/* --- version non-empty --- */
SELECT length(yaml_version()) > 0;
