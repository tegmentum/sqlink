.load extensions/ngrams/target/wasm32-wasip2/release/ngrams_extension.component.wasm

/* Smoke test for the `ngrams` extension.
 * Run via:  tooling/smoke.py ngrams
 *
 * Covers: char + word n-grams, count scalars, edge cases
 * (n > len → []), Unicode (combining-free CJK + whitespace
 * collapsing), and NULL propagation. */

/* ─── ngrams_char ───
 * "abcd" with n=2 → ["ab","bc","cd"]; n=3 → ["abc","bcd"]. */
SELECT ngrams_char('abcd', 2);
SELECT ngrams_char('abcd', 3);
SELECT ngrams_char('abcd', 4);

/* n equal to char count → one window. n > char count → []. */
SELECT ngrams_char('abc', 3);
SELECT ngrams_char('abc', 4);

/* n=1 degenerates to a per-char tokenizer. */
SELECT ngrams_char('hi!', 1);

/* Unicode scalar code points: CJK and emoji are one char each.
 * "日本語" with n=2 → ["日本","本語"]. */
SELECT ngrams_char('日本語', 2);

/* ─── ngrams_word ───
 * "the quick brown fox" with n=2 →
 *   ["the quick","quick brown","brown fox"]. */
SELECT ngrams_word('the quick brown fox', 2);
SELECT ngrams_word('the quick brown fox', 3);

/* Unicode whitespace collapsing: tabs + multiple spaces fold to
 * single-word separators. Output joins with single ' '. */
SELECT ngrams_word('a  b	c', 2);

/* n > word count → []. */
SELECT ngrams_word('one two', 3);

/* ─── count scalars ───
 * Char count matches len(json) - n + 1. */
SELECT ngrams_count_char('abcd', 2);
SELECT ngrams_count_char('abcd', 4);
SELECT ngrams_count_char('abcd', 5);
SELECT ngrams_count_char('日本語', 2);

/* Word count = word_count - n + 1. */
SELECT ngrams_count_word('the quick brown fox', 2);
SELECT ngrams_count_word('the quick brown fox', 4);
SELECT ngrams_count_word('one two', 3);

/* Empty string: 0 windows for any n ≥ 1. */
SELECT ngrams_count_char('', 1);
SELECT ngrams_count_word('', 1);
SELECT ngrams_char('', 2);
SELECT ngrams_word('', 2);

/* ─── NULL propagation ───
 * Either arg NULL → NULL. */
SELECT ngrams_char(NULL, 2);
SELECT ngrams_char('abc', NULL);
SELECT ngrams_word(NULL, 2);
SELECT ngrams_word('a b c', NULL);
SELECT ngrams_count_char(NULL, 2);
SELECT ngrams_count_word(NULL, 2);

/* ─── version ─── */
SELECT length(ngrams_version()) > 0;
