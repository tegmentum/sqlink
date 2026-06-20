.load extensions/mbox/target/wasm32-wasip2/release/mbox_extension.component.wasm

/* Build a two-message mbox-O text blob in a CTE; reuse it across
 * the rest of the smoke. char(10) is LF; mbox is line-oriented. */
CREATE TEMP TABLE m AS
SELECT
  'From alice@example.com Sun Oct  2 14:06:22 2016' || char(10) ||
  'From: Alice <alice@example.com>'                 || char(10) ||
  'To: Bob <bob@example.com>'                       || char(10) ||
  'Subject: Hello Bob'                              || char(10) ||
  'Date: Sun, 02 Oct 2016 14:06:22 +0000'           || char(10) ||
  'Content-Type: text/plain; charset=us-ascii'      || char(10) ||
  ''                                                || char(10) ||
  'Hi Bob, just saying hi.'                         || char(10) ||
  'See you soon.'                                   || char(10) ||
  ''                                                || char(10) ||
  'From carol@example.com Mon Oct  3 09:30:00 2016' || char(10) ||
  'From: Carol <carol@example.com>'                 || char(10) ||
  'To: Dave <dave@example.com>'                     || char(10) ||
  'Subject: Lunch?'                                 || char(10) ||
  'Date: Mon, 03 Oct 2016 09:30:00 +0000'           || char(10) ||
  ''                                                || char(10) ||
  'Are you free for lunch today?'                   || char(10)
AS box;

/* mbox_message_count -> 2 */
SELECT mbox_message_count(box) FROM m;

/* mbox_subjects -> JSON array of 2 subjects */
SELECT mbox_subjects(box) FROM m;

/* Per-index accessors for message 0 */
SELECT mbox_from_at(box, 0) FROM m;
SELECT mbox_subject_at(box, 0) FROM m;
SELECT mbox_date_at(box, 0) FROM m;

/* Per-index accessors for message 1 */
SELECT mbox_from_at(box, 1) FROM m;
SELECT mbox_subject_at(box, 1) FROM m;
SELECT mbox_date_at(box, 1) FROM m;

/* Body extraction: first message body starts with the greeting. */
SELECT instr(mbox_body_at(box, 0), 'Hi Bob') > 0 FROM m;
SELECT instr(mbox_body_at(box, 1), 'lunch today') > 0 FROM m;

/* Raw RFC 822 message_at should contain the Subject header line. */
SELECT instr(mbox_message_at(box, 0), 'Subject: Hello Bob') > 0 FROM m;

/* Out-of-range index -> NULL on all per-index accessors. */
SELECT mbox_message_at(box, 5) IS NULL FROM m;
SELECT mbox_from_at(box, 99) IS NULL FROM m;
SELECT mbox_subject_at(box, -1) IS NULL FROM m;

/* NULL input -> NULL out, no error. */
SELECT mbox_message_count(NULL) IS NULL;
SELECT mbox_subjects(NULL) IS NULL;
SELECT mbox_body_at(NULL, 0) IS NULL;

/* Empty string -> count 0, empty subjects array. */
SELECT mbox_message_count('');
SELECT mbox_subjects('');

/* Non-mbox text (no From_ line) -> count 0. */
SELECT mbox_message_count('just some random text' || char(10) || 'no envelope here');

/* mbox-rd: body lines starting with >From get one '>' stripped. */
SELECT mbox_body_at(
  'From sender@example.com Tue Jan  1 00:00:00 2019' || char(10) ||
  'From: sender@example.com'                         || char(10) ||
  'Subject: rd test'                                 || char(10) ||
  ''                                                 || char(10) ||
  '>From the depths'                                 || char(10) ||
  '>>From further down'                              || char(10),
  0);

/* Version is non-empty. */
SELECT length(mbox_version()) > 0;
