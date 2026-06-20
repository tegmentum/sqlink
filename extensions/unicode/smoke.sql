.load extensions/unicode/target/wasm32-wasip2/release/unicode_extension.component.wasm

/* === Normalization ===
 * 'e' + U+0301 combining acute accent normalizes to U+00E9. We
 * compare via hex() to keep the smoke harness's text diff stable
 * across terminals. NFC of (0x65, 0xCC, 0x81) = (0xC3, 0xA9). */
SELECT hex(unicode_nfc(char(0x65) || char(0x0301)));

/* NFD round-trip: 'é' (U+00E9) decomposes to (0x65, 0xCC, 0x81). */
SELECT hex(unicode_nfd(char(0x00E9)));

/* NFC of NFD is identity for ASCII. */
SELECT unicode_nfc(unicode_nfd('hello'));

/* NFKC + NFKD smoke: 'ﬁ' (U+FB01 LATIN SMALL LIGATURE FI) decomposes
 * to "fi" under compatibility. */
SELECT unicode_nfkc(char(0xFB01));
SELECT unicode_nfkd(char(0xFB01));

/* === Case folding ===
 * 'Straße' (U+00DF eszett) folds to "strasse" under full Unicode
 * case folding. Rust's stdlib to_lowercase() would NOT  it leaves
 * ß as ß. This is the headline acceptance criterion. */
SELECT unicode_fold('Stra' || char(0x00DF) || 'e');

/* === Accent stripping ===
 * 'café' -> "cafe"; 'naïve' -> "naive". */
SELECT unicode_strip_accents('caf' || char(0x00E9));
SELECT unicode_strip_accents('na' || char(0x00EF) || 've');

/* === Slugify ===
 * The plan's headline criterion. */
SELECT unicode_slugify('Hello, World!');
/* 'café é à' -> "cafe-e-a" */
SELECT unicode_slugify('caf' || char(0x00E9) || ' ' || char(0x00E9) || ' ' || char(0x00E0));
/* Heavy punctuation + leading/trailing whitespace -> clean slug. */
SELECT unicode_slugify('  ---Hello,   World!!! ---  ');

/* === Whitespace normalization ===
 * Run of tabs + spaces + newlines collapses to single space; trim. */
SELECT unicode_normalize_whitespace('  hello   ' || char(0x09) || '  world  ');

/* === Category ===
 * 'A' is Lu, 'a' is Ll, '5' is Nd, ' ' is Zs (space separator). */
SELECT unicode_category('A');
SELECT unicode_category('a');
SELECT unicode_category('5');
SELECT unicode_category(' ');

/* === Grapheme count ===
 * 'é' as NFD (e + combining acute) is one grapheme. */
SELECT unicode_grapheme_count(char(0x65) || char(0x0301));
/* US flag emoji (U+1F1FA U+1F1F8) is ONE grapheme even though it's
 * two regional-indicator codepoints. */
SELECT unicode_grapheme_count(char(0x1F1FA) || char(0x1F1F8));
/* Empty string -> 0. */
SELECT unicode_grapheme_count('');

/* === NULL passthrough ===
 * NULL in -> NULL out on each scalar. */
SELECT unicode_nfc(NULL) IS NULL;
SELECT unicode_slugify(NULL) IS NULL;
SELECT unicode_grapheme_count(NULL) IS NULL;

/* === Version ===
 * Non-empty, contains "Unicode" substring. */
SELECT length(unicode_version()) > 0;
SELECT instr(unicode_version(), 'Unicode') > 0;
