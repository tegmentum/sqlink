.load extensions/sequence/target/wasm32-wasip2/release/sequence_extension.component.wasm
SELECT nextval('s1'), nextval('s1'), nextval('s1');
SELECT currval('s1');
SELECT setval('s1', 100);
SELECT nextval('s1');
