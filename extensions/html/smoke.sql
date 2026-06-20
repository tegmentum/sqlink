.load extensions/html/target/wasm32-wasip2/release/html_extension.component.wasm

/* ---- html_to_text: tags stripped, entities decoded ---- */
SELECT html_to_text('<p>hi</p>');
SELECT html_to_text('<p>a&amp;b</p>');

/* ---- html_get_text: CSS selector picks the first match ---- */
SELECT html_get_text('<p class="x">a</p><p>b</p>', '.x');
SELECT html_get_text('<p class="x">a</p><p>b</p>', 'p');

/* ---- html_get_attr: first match's attribute value ---- */
SELECT html_get_attr('<a href="/x">L</a>', 'a', 'href');
SELECT html_get_attr('<a href="/x">L</a>', 'a', 'missing');

/* ---- html_all_text: JSON array of all matches' text ---- */
SELECT html_all_text('<li>a</li><li>b</li><li>c</li>', 'li');

/* ---- entity codec round-trip ---- */
SELECT html_decode_entities('&lt;b&gt;hi&amp;');
SELECT html_encode_entities('<b>');

/* ---- raw tag-strip (no entity decode) ---- */
SELECT html_strip_tags('<p>a&amp;b</p>');

/* ---- links / images / title ---- */
SELECT html_links('<a href="/a">L</a><a href="/b">M</a>');
SELECT html_images('<img src="/i.png" alt="cat"><img src="/j.png">');
SELECT html_title('<html><head><title>T</title></head></html>');

/* ---- NULL propagation ---- */
SELECT html_to_text(NULL) IS NULL;
SELECT html_get_text(NULL, 'p') IS NULL;
SELECT html_decode_entities(NULL) IS NULL;
SELECT html_links(NULL) IS NULL;

/* ---- malformed HTML still extracts text (html5ever is liberal) ---- */
SELECT html_to_text('<p>unclosed <b>bold');

/* ---- version is a non-empty TEXT ---- */
SELECT length(html_version()) > 0;
