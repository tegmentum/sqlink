.load extensions/lemmatize/target/wasm32-wasip2/release/lemmatize_extension.component.wasm

/* ─── brief acceptance cases ─── */
SELECT lemmatize('running');                   -- expected: run
SELECT lemmatize('better');                    -- expected: good (adj exc hits before verb rules)
SELECT lemmatize_pos('was', 'v');              -- expected: be
SELECT lemmatize_pos('better', 'adj');         -- expected: good
SELECT lemmatize_pos('running', 'v');          -- expected: run

/* ─── POS aliases work ─── */
SELECT lemmatize_pos('children', 'n');         -- expected: child
SELECT lemmatize_pos('children', 'noun');      -- expected: child

/* ─── regular inflection via rules ─── */
SELECT lemmatize_pos('walks', 'v');            -- expected: walk
SELECT lemmatize_pos('walked', 'v');           -- expected: walk
SELECT lemmatize_pos('countries', 'n');        -- expected: country
SELECT lemmatize_pos('dogs', 'n');             -- expected: dog

/* ─── adverb path: "better" as adv falls back to "well" exc ─── */
SELECT lemmatize_pos('better', 'adv');         -- expected: well

/* ─── mixed-case input is normalised before lookup ─── */
SELECT lemmatize('RUNNING');                   -- expected: run
SELECT lemmatize('Was', 'en');                 -- expected: be

/* ─── default lang is en ─── */
SELECT lemmatize('was');                       -- expected: be

/* ─── non-english falls through to stem ─── */
SELECT lemmatize('laufen', 'de');              -- expected: lauf (snowball german)
SELECT lemmatize('manger', 'fr');              -- expected: mang (snowball french)

/* ─── languages and version surfaces ─── */
SELECT lemmatize_languages();                  -- expected: JSON array
SELECT length(lemmatize_version()) > 0;        -- expected: 1

/* ─── NULL propagation ─── */
SELECT lemmatize(NULL);                        -- expected: <NULL>
SELECT lemmatize(NULL, 'en');                  -- expected: <NULL>
SELECT lemmatize('running', NULL);             -- expected: <NULL>
SELECT lemmatize_pos(NULL, 'v');               -- expected: <NULL>
SELECT lemmatize_pos('was', NULL);             -- expected: <NULL>
SELECT lemmatize_pos('was', 'v', NULL);        -- expected: <NULL>
