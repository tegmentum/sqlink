.load extensions/mailto/target/wasm32-wasip2/release/mailto_extension.component.wasm

SELECT mailto_validate('mailto:alice@example.com');
SELECT mailto_validate('not a mailto');
SELECT mailto_to('mailto:alice@example.com?subject=Hello&body=Hi%20there');
SELECT mailto_subject('mailto:alice@example.com?subject=Hello%20World');
SELECT mailto_body('mailto:alice@example.com?subject=Hello&body=Hi%20there');
SELECT mailto_cc('mailto:alice@example.com?cc=bob@example.com');
SELECT mailto_recipients('mailto:alice@example.com,bob@example.com?to=carol@example.com');
