.load extensions/json-table/target/wasm32-wasip2/release/json_table_extension.component.wasm
SELECT idx, key, value FROM json_table('{"items":[{"id":1,"name":"a"},{"id":2,"name":"b"}]}', '$.items');
SELECT key, value FROM json_table('{"a":1,"b":2,"c":3}', '$');
