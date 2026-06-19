-- cli: baked
-- Coexistence regression: baked + .load extensions in the same
-- session. Uses sqlite_cli_baked.component.wasm built with sha3
-- + uuid baked in (see PLAN-bake-in.md, run
-- `sqlite-wasm-run compose --bake sha3,uuid` first).
--
-- Then .load's a second component (eval) to prove dynamic load
-- isn't broken when the cli already has baked-in extensions.

-- baked sha3_256: no .load needed
SELECT length(sha3_256('hello')) = 64;

-- baked uuidv4 + uuid_validate
SELECT uuid_validate(uuidv4()) = 1;

-- composition of two baked scalars
SELECT length(sha3_256(uuidv4())) = 64;

-- now bring in a non-baked extension via .load
.load extensions/eval/target/wasm32-wasip2/release/eval_extension.component.wasm

-- the .load'd surface works...
SELECT eval('SELECT 7');

-- ...alongside the baked surface, in one statement
SELECT length(eval('SELECT ''' || sha3_256('x') || '''')) = 64;
