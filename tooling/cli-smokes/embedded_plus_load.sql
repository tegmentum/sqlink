-- cli: embedded
-- Coexistence regression: embedded + .load extensions in the same
-- session. Uses sqlite_cli_embedded.component.wasm built with
-- sha3 + uuid embedded (see PLAN-embed-extensions.md, run
-- `sqlite-wasm-run compose --embed sha3,uuid` first).
--
-- Then .load's a second component (eval) to prove dynamic load
-- isn't broken when the cli already has embedded extensions.

-- embedded sha3_256: no .load needed
SELECT length(sha3_256('hello')) = 64;

-- embedded uuidv4 + uuid_validate
SELECT uuid_validate(uuidv4()) = 1;

-- composition of two embedded scalars
SELECT length(sha3_256(uuidv4())) = 64;

-- now bring in a non-embedded extension via .load
.load extensions/eval/target/wasm32-wasip2/release/eval_extension.component.wasm

-- the .load'd surface works...
SELECT eval('SELECT 7');

-- ...alongside the embedded surface, in one statement
SELECT length(eval('SELECT ''' || sha3_256('x') || '''')) = 64;
