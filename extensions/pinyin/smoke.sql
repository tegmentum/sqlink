.load extensions/pinyin/target/wasm32-wasip2/release/pinyin_extension.component.wasm

/* ─── pinyin(): plain pinyin, no tones ─── */
SELECT pinyin('中国');
SELECT pinyin('中国人');
SELECT pinyin('中文');
SELECT pinyin('拼音');
SELECT pinyin('你好');
SELECT pinyin('汉语');

/* ─── pinyin_with_tone(): numeric tone after the vowel ─── */
SELECT pinyin_with_tone('中国');
SELECT pinyin_with_tone('你好');
SELECT pinyin_with_tone('汉语');

/* ─── pinyin_with_diacritic(): unicode tone marks ─── */
SELECT pinyin_with_diacritic('中国');
SELECT pinyin_with_diacritic('你好');
SELECT pinyin_with_diacritic('汉语');

/* ─── pinyin_first_letter(): initials ─── */
SELECT pinyin_first_letter('中文');
SELECT pinyin_first_letter('中国人');
SELECT pinyin_first_letter('拼音');

/* ─── pinyin_split(): JSON array of per-char pinyin ─── */
SELECT pinyin_split('中国');
SELECT pinyin_split('你好');
SELECT pinyin_split('拼音');

/* ─── pinyin_is_chinese(): 1 if any CJK char, 0 otherwise ─── */
SELECT pinyin_is_chinese('中国');
SELECT pinyin_is_chinese('hello 世界');
SELECT pinyin_is_chinese('hello');
SELECT pinyin_is_chinese('');

/* ─── mixed: ASCII passes through; pre-existing space not doubled ─── */
SELECT pinyin('你好 world');
SELECT pinyin('hello');

/* ─── NULL propagation across the full surface ─── */
SELECT pinyin(NULL);
SELECT pinyin_with_tone(NULL);
SELECT pinyin_with_diacritic(NULL);
SELECT pinyin_first_letter(NULL);
SELECT pinyin_split(NULL);
SELECT pinyin_is_chinese(NULL);

/* ─── pinyin_version() is non-empty ─── */
SELECT length(pinyin_version()) > 0;
