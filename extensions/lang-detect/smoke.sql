.load extensions/lang-detect/target/wasm32-wasip2/release/lang_detect_extension.component.wasm

/* ---- Basic English / French detection (PLAN acceptance).
 *
 * "The quick brown fox..." is the canonical English pangram --
 * trigram-based n-gram detectors should land 'eng' easily. */
SELECT lang_detect('The quick brown fox jumps over the lazy dog');
SELECT lang_detect_alpha2('The quick brown fox jumps over the lazy dog');

/* French paragraph (>= 20 chars so the PLAN's "unreliable below
 * ~20 chars" caveat doesn't apply). */
SELECT lang_detect('Le rapide renard brun saute par-dessus le chien paresseux. Cette phrase est en francais et devrait etre detectee comme telle.');
SELECT lang_detect_alpha2('Le rapide renard brun saute par-dessus le chien paresseux. Cette phrase est en francais et devrait etre detectee comme telle.');

/* ---- Short-string guard (PLAN caveat + acceptance).
 *
 * Strings < 3 chars are flatly NULLed; whatlang otherwise happily
 * labels 'a' / 'ab' as some random trigram-matching language. */
SELECT lang_detect('');
SELECT lang_detect('a');
SELECT lang_detect('ab');
SELECT lang_detect_alpha2('a');
SELECT lang_detect_confidence('a');
SELECT lang_detect_all('a');

/* ---- Script detection (PLAN acceptance).
 *
 * Empty -> NULL; otherwise dominant-script wins. We rename
 * whatlang's `Mandarin` to `Han` (ISO 15924) per the PLAN. */
SELECT lang_detect_script('');
SELECT lang_detect_script('Привет мир, это русский');
SELECT lang_detect_script('中文测试');
SELECT lang_detect_script('The quick brown fox jumps over the lazy dog');
SELECT lang_detect_script('مرحبا بالعالم');

/* Single-char scripts are still unambiguous -- script detection
 * has no min-length guard (unlike the lang scalars). */
SELECT lang_detect_script('A');

/* ---- Confidence: 0..1, deterministic for fixed input. */
SELECT lang_detect_confidence('The quick brown fox jumps over the lazy dog. This text is in English language for testing purposes. The detector should be very confident.') > 0.5;
SELECT lang_detect_confidence('The quick brown fox jumps over the lazy dog. This text is in English language for testing purposes. The detector should be very confident.') <= 1.0;

/* ---- lang_detect_all: JSON array of <= 3 candidates.
 *
 * Approximation, top candidate is well-defined; secondary
 * candidates come from a denylist re-run so their confidence is
 * computed against a reduced pool (documented in lib.rs). */
SELECT json_array_length(lang_detect_all('The quick brown fox jumps over the lazy dog. This text is in English language for testing purposes.'));
SELECT json_extract(lang_detect_all('The quick brown fox jumps over the lazy dog. This text is in English language for testing purposes.'), '$[0].lang');
SELECT json_type(lang_detect_all('The quick brown fox jumps over the lazy dog. This text is in English language for testing purposes.'));

/* ---- lang_supported: PLAN says >= 50; whatlang ships 70. */
SELECT json_array_length(lang_supported()) >= 50;
SELECT json_array_length(lang_supported());
SELECT json_extract(lang_supported(), '$[0]');

/* ---- Version string is non-empty. */
SELECT length(lang_detect_version()) > 0;
