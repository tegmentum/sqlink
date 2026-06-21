.load extensions/sys-compat/target/wasm32-wasip2/release/sys_compat_extension.component.wasm

SELECT user();
SELECT current_user();
SELECT session_user();
SELECT system_user();
SELECT '[' || current_role() || ']';
SELECT database();
SELECT current_database();
SELECT schema();
SELECT current_schema();
SELECT current_schemas(0);
SELECT current_schemas(1);
SELECT version();
SELECT collation('hello');
SELECT format_bytes(0);
SELECT format_bytes(512);
SELECT format_bytes(2048);
SELECT format_bytes(1048576);
SELECT format_bytes(1500000000);
SELECT format_bytes(-2048);
