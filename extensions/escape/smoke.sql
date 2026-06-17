.load extensions/escape/target/wasm32-wasip2/release/escape_extension.component.wasm

/* Smoke test for the `escape` extension.
 * Run via:  tooling/smoke.py escape
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

SELECT url_encode('hello world');
SELECT url_encode('a/b=c&d');
SELECT url_decode('hello%20world');
SELECT url_decode('a%2Fb%3Dc%26d');
SELECT url_decode('a+b');
SELECT html_escape('<script>alert("x")</script>');
SELECT html_unescape('&lt;b&gt;hi&amp;bye&lt;/b&gt;');
SELECT html_unescape('&#65;&#66;&#x43;');
SELECT sql_quote('it''s fine');
SELECT shell_quote('echo $PATH; rm -rf /');
