.load extensions/numfmt/target/wasm32-wasip2/release/numfmt_extension.component.wasm

/* Commas + fixed places. */
SELECT numfmt_commas(1234567.891, 2);
SELECT numfmt_commas(-1234.5, 1);
SELECT numfmt_commas(999, 0);
SELECT numfmt_commas(0, 3);
SELECT numfmt_fixed(3.14159, 2);
SELECT numfmt_fixed(3.14159, 0);

/* Ordinal: handles 11/12/13 = -th regardless of last digit. */
SELECT numfmt_ordinal(1);
SELECT numfmt_ordinal(2);
SELECT numfmt_ordinal(3);
SELECT numfmt_ordinal(11);    /* "11th"  NOT "11st" */
SELECT numfmt_ordinal(21);
SELECT numfmt_ordinal(112);
SELECT numfmt_ordinal(0);

/* Scientific. */
SELECT numfmt_scientific(1234.5, 3);
SELECT numfmt_scientific(0.000123, 2);

/* Percent. */
SELECT numfmt_percent(0.135, 1);
SELECT numfmt_percent(1.0, 0);
SELECT numfmt_percent(-0.05, 2);

/* Left-pad. */
SELECT numfmt_pad_left('42', 5, '0');
SELECT numfmt_pad_left('hi', 4, '.');   /* visible fill: parser strips ws */
SELECT numfmt_pad_left('toolong', 3, '0');

/* European grouping with custom separator. */
SELECT numfmt_group(1234567.89, '.');
