.load json-table
SELECT idx, key, value FROM json_table('{"items":[{"id":1,"name":"a"},{"id":2,"name":"b"}]}', '$.items');
SELECT key, value FROM json_table('{"a":1,"b":2,"c":3}', '$');
