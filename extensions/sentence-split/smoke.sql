.load extensions/sentence-split/target/wasm32-wasip2/release/sentence_split_extension.component.wasm

/* ─── basic split: two simple sentences ─── */
SELECT split_sentences('Hello world. How are you?');
SELECT sentence_count('Hello world. How are you?');

/* ─── abbreviation guard: "Mr." doesn't split ─── */
SELECT sentence_count('Mr. Smith went home. He smiled.');

/* ─── initialism guard: "U.S.A." doesn't split mid-sentence ─── */
SELECT sentence_count('The U.S.A. is large. Canada too.');

/* ─── decimal guard: "3.14" doesn't split ─── */
SELECT sentence_count('Pi is about 3.14. Cool.');

/* ─── quoted terminator absorbed into preceding sentence ─── */
SELECT sentence_count('He said "go!" then left. Done.');

/* ─── multi-terminator run: "?!" counts as one boundary ─── */
SELECT sentence_count('Really?! No way.');

/* ─── trailing sentence without terminator still counted ─── */
SELECT sentence_count('First sentence. Second one');

/* ─── explicit lang arg works the same as default ─── */
SELECT sentence_count('Hello world. Bye world.', 'en');
SELECT sentence_count('Hello world. Bye world.', 'english');

/* ─── split_sentences_with_indices: JSON of {sentence,start,end} ─── */
SELECT split_sentences_with_indices('Hi. Bye.');

/* ─── NULL propagation: text NULL, lang NULL ─── */
SELECT split_sentences(NULL);
SELECT sentence_count(NULL);
SELECT split_sentences_with_indices(NULL);
SELECT split_sentences('Hi.', NULL);

/* ─── empty input ─── */
SELECT split_sentences('');
SELECT sentence_count('');

/* ─── version non-empty ─── */
SELECT length(sentence_split_version()) > 0;
