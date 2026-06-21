.load extensions/unnest/target/wasm32-wasip2/release/unnest_extension.component.wasm
SELECT idx, value FROM unnest('[10,"alpha",true,null]');
SELECT count(*) FROM unnest('[1,2,3,4,5,6,7,8,9,10]');
