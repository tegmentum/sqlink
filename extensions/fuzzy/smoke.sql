.load extensions/fuzzy/target/wasm32-wasip2/release/fuzzy_extension.component.wasm

/* ─── Jaro / Jaro-Winkler ───
 * The canonical Winkler 1990 paper example:
 *   jaro("MARTHA","MARHTA")        = 0.9444...
 *   jaro_winkler("MARTHA","MARHTA")= 0.9611...   (within 0.001 of 0.961)
 * round() pins the float text shape across cli formatters. */
SELECT round(jaro('MARTHA','MARHTA'), 4);
SELECT round(jaro_winkler('MARTHA','MARHTA'), 4);

/* abs(jaro_winkler - 0.961) < 0.001 — the plan acceptance line. */
SELECT abs(jaro_winkler('MARTHA','MARHTA') - 0.961) < 0.001;

/* DIXON / DICKSONX — another classic Winkler vector. */
SELECT round(jaro_winkler('DIXON','DICKSONX'), 4);

/* Identical strings → 1.0; disjoint → 0.0. SQLite renders these
 * as integer-shaped text ("1", "0"). */
SELECT jaro_winkler('abc','abc');
SELECT jaro_winkler('abc','xyz');

/* ─── Damerau-Levenshtein vs Levenshtein ───
 * Plan acceptance: damerau_levenshtein("CA","ABC") == 2. Plain
 * Levenshtein for the same pair is 3 (no transposition shortcut). */
SELECT damerau_levenshtein('CA','ABC');
SELECT levenshtein('CA','ABC');

/* The discriminator vector: "ab" vs "ba" — Damerau collapses to 1
 * (single transposition), plain Levenshtein still costs 2. */
SELECT damerau_levenshtein('ab','ba');
SELECT levenshtein('ab','ba');

/* Classic Levenshtein vectors. */
SELECT levenshtein('kitten','sitting');
SELECT levenshtein('saturday','sunday');

/* Equal strings → 0. */
SELECT levenshtein('abc','abc');
SELECT damerau_levenshtein('abc','abc');

/* ─── Soundex ───
 * US Census 1880 reference:
 *   "Robert" → "R163"
 *   "Rupert" → "R163"   (intentional collision; plan acceptance)
 *   "Tymczak" → "T522"
 *   "Honeyman" → "H555" */
SELECT soundex('Robert');
SELECT soundex('Rupert');
SELECT soundex('Tymczak');
SELECT soundex('Honeyman');

/* Same code → SQL-level collision (the whole point of soundex). */
SELECT soundex('Robert') = soundex('Rupert');

/* ─── Metaphone ───
 * rphonetic (commons-codec port) outputs:
 *   "Pittsburgh" → "PTSB"
 *   "Thompson"   → "0MPS"  (TH digraph → '0')
 *   "Smith"      → "SM0" */
SELECT metaphone('Pittsburgh');
SELECT metaphone('Thompson');
SELECT metaphone('Smith');

/* ─── Double Metaphone ───
 * Lawrence Philips's original Double Metaphone produces a primary
 * and a secondary code; rphonetic exposes both. The plan calls
 * out the canonical "Smith" pair: ("SM0", "XMT"). */
SELECT double_metaphone_primary('Smith');
SELECT double_metaphone_secondary('Smith');

/* German-origin name: "Schmidt" — primary = XMT, secondary = SMT. */
SELECT double_metaphone_primary('Schmidt');
SELECT double_metaphone_secondary('Schmidt');

/* ─── Caverphone (2.0 — the 2004 revision; 10 chars padded with 1s) ─── */
SELECT caverphone('Thompson');
SELECT caverphone('Smith');
SELECT caverphone('Pittsburgh');

/* ─── Empty + NULL ───
 * Plan acceptance: explicit empty + NULL handling.
 * Empty string → encoder-specific empty-form output (NOT NULL).
 * Wrap the soundex/metaphone/double-metaphone empties in brackets
 * because rphonetic returns an empty string for those, and the
 * smoke harness drops bare blank lines. Caverphone 2.0 always pads
 * to 10 chars, so its empty-input output is visible by itself. */
SELECT '[' || soundex('') || ']';
SELECT '[' || metaphone('') || ']';
SELECT '[' || double_metaphone_primary('') || ']';
SELECT '[' || double_metaphone_secondary('') || ']';
SELECT caverphone('');

/* NULL → NULL on every scalar. */
SELECT jaro(NULL, 'x');
SELECT jaro('x', NULL);
SELECT jaro_winkler(NULL, 'x');
SELECT damerau_levenshtein(NULL, 'x');
SELECT levenshtein('x', NULL);
SELECT soundex(NULL);
SELECT metaphone(NULL);
SELECT double_metaphone_primary(NULL);
SELECT double_metaphone_secondary(NULL);
SELECT caverphone(NULL);

/* Version is non-empty. */
SELECT length(fuzzy_version()) > 0;
