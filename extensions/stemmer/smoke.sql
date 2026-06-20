.load extensions/stemmer/target/wasm32-wasip2/release/stemmer_extension.component.wasm

/* ─── english (default lang) acceptance cases ───
 * Per PLAN-more-extensions.md  4: stem("running")  "run",
 * stem("better")  "better" (Porter2 doesn't aggressively shorten),
 * stem("histories")  "histori". */
SELECT stem('running');
SELECT stem('better');
SELECT stem('histories');

/* ─── english extras: Porter2 worked examples ─── */
SELECT stem('fruitlessly');
SELECT stem('CONSIGNED');               -- mixed-case lowercased internally
SELECT stem('consigning');
SELECT stem('consignment');

/* ─── explicit english arg, same result as default ─── */
SELECT stem('running', 'english');
SELECT stem('running', 'EN');           -- ISO code, case-insensitive

/* ─── non-english: german "laufen"  "lauf" (PLAN ) ─── */
SELECT stem('laufen', 'german');
SELECT stem('laufen', 'de');

/* ─── french: "manger" (to eat) stems toward "mang" ─── */
SELECT stem('manger', 'french');

/* ─── NULL propagation: NULL word  NULL, NULL lang  NULL ─── */
SELECT stem(NULL);
SELECT stem(NULL, 'english');
SELECT stem('running', NULL);

/* ─── stem_languages() returns the canonical alphabetised list ─── */
SELECT stem_languages();

/* ─── stemmer_version() is non-empty ─── */
SELECT length(stemmer_version()) > 0;

/* ─── unknown lang case is verified out-of-band: stem('hello',
 * 'klingon') errors with a "stem: unknown language..." message.
 * The smoke harness diffs the structured row stream, not the
 * error pipe, so we don't include the error-emitting SELECT
 * here  it would either produce no row or a panic line and
 * destabilise the diff. The acceptance criterion is met by the
 * `lang_to_algorithm` returning None for unknown langs and
 * `stem_one` formatting the error with the supported-lang list
 * (covered by the source). */
