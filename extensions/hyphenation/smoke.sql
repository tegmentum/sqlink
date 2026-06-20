.load extensions/hyphenation/target/wasm32-wasip2/release/hyphenation_extension.component.wasm

/* The "hyphenation" extension exposes 4 SQL surfaces (some with
 * 1-arg + 2-arg overloads): hyphenate, hyphenate_positions,
 * hyphenation_languages, hyphenation_version. The smoke covers
 * each — happy paths, defaults, null propagation, error cases.
 *
 * Soft-hyphen (U+00AD) is invisible in most renderers but is
 * literally present in the output text. We assert against its
 * presence by counting it with length()/replace() rather than
 * trying to pin a literal byte sequence in smoke.expected.
 */

/* ─── hyphenate (default lang = en-us) ─── */
/* "hyphenation" has 3 break opportunities in en-us patterns:
 *   hy-phen-a-tion (positions [2, 6, 7])
 * → 4 segments → 3 SHYs inserted → length grows by 3 chars. */
SELECT length(hyphenate('hyphenation')) - length('hyphenation');

/* The hyphenated text decomposes into 4 segments when we split
 * on U+00AD. char(173) is U+00AD; sqlite's char() takes codepoints. */
SELECT replace(hyphenate('hyphenation'), char(173), '|');

/* Explicit lang arg matches default. */
SELECT hyphenate('hyphenation', 'en-us') = hyphenate('hyphenation');

/* Mixed-case lang tag works — we lowercase before lookup. */
SELECT hyphenate('hyphenation', 'EN-US') = hyphenate('hyphenation');

/* NULL lang arg = use default. */
SELECT hyphenate('hyphenation', NULL) = hyphenate('hyphenation');

/* Short word (below en-us lefthyphenmin=2 / righthyphenmin=3
 * combined minimum of 5 letters) → no breaks, unchanged. */
SELECT hyphenate('cat');
SELECT length(hyphenate('cat')) = length('cat');

/* ─── hyphenate_positions (JSON array of byte offsets) ─── */
/* Classic en-us example from the upstream crate's README:
 *   "hyphenation" → [2, 6, 7]   (hy|phen|a|tion) */
SELECT hyphenate_positions('hyphenation');

/* "anfractuous" → [2, 6, 8]  (an|frac|tu|ous), the crate's
 * doctest example. */
SELECT hyphenate_positions('anfractuous');

/* Short word → empty array (not null). */
SELECT hyphenate_positions('cat');

/* json_array_length confirms the array shape — 3 breaks for
 * "hyphenation". */
SELECT json_array_length(hyphenate_positions('hyphenation'));

/* Explicit + default lang produce identical positions. */
SELECT hyphenate_positions('hyphenation', 'en-us') = hyphenate_positions('hyphenation');

/* ─── hyphenation_languages ─── */
/* This build embeds only en-us by default (saves ~3 MB vs
 * embed_all). The list MUST contain "en-us" at minimum. */
SELECT json_array_length(hyphenation_languages()) >= 1;
SELECT json_extract(hyphenation_languages(), '$[0]');

/* ─── hyphenation_version ─── */
SELECT length(hyphenation_version()) > 0;

/* ─── NULL propagation ─── */
/* NULL word → NULL on both content scalars. */
SELECT hyphenate(NULL);
SELECT hyphenate(NULL, 'en-us');
SELECT hyphenate_positions(NULL);
SELECT hyphenate_positions(NULL, 'en-us');

/* ─── Error: unknown / non-embedded lang ─── */
/* "de" is a valid BCP-47 tag the upstream crate knows about, but
 * we didn't embed German patterns. The lookup MUST fail loudly
 * rather than silently fall back to en-us. */
SELECT hyphenate('hyphenation', 'de');
