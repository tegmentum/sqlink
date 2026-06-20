.load extensions/useragent/target/wasm32-wasip2/release/useragent_extension.component.wasm

/* ────────────── Chrome on Linux ──────────────
 * Acceptance: ua_browser=="Chrome", ua_os=="Linux". */
SELECT ua_browser('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36');
SELECT ua_browser_version('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36');
SELECT ua_os('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36');
SELECT ua_is_bot('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36');

/* ────────────── Safari on iOS ──────────────
 * Acceptance: ua_browser=="Safari", ua_os=="iPhone",
 * ua_device=="iPhone". */
SELECT ua_browser('Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1');
SELECT ua_os('Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1');
SELECT ua_os_version('Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1');
SELECT ua_device('Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1');
SELECT ua_is_bot('Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1');

/* ────────────── Firefox on macOS ──────────────
 * Acceptance: parses cleanly. */
SELECT ua_browser('Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:120.0) Gecko/20100101 Firefox/120.0');
SELECT ua_browser_version('Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:120.0) Gecko/20100101 Firefox/120.0');
SELECT ua_os('Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:120.0) Gecko/20100101 Firefox/120.0');
SELECT ua_os_version('Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:120.0) Gecko/20100101 Firefox/120.0');
SELECT ua_is_bot('Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:120.0) Gecko/20100101 Firefox/120.0');

/* ────────────── googlebot ──────────────
 * Acceptance: ua_is_bot==1. */
SELECT ua_is_bot('Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)');
SELECT ua_browser('Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)');

/* ────────────── ua_parse (full JSON) on Chrome/Linux ──────────── */
SELECT ua_parse('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36');

/* ua_parse on googlebot to lock down the crawler shape. */
SELECT ua_parse('Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)');

/* ────────────── NULL passthrough ──────────────
 * Every parser scalar maps NULL  NULL. The smoke harness sets
 * `.nullvalue <NULL>` so a NULL result emits the literal token. */
SELECT ua_browser(NULL);
SELECT ua_os(NULL);
SELECT ua_is_bot(NULL);
SELECT ua_parse(NULL);

/* ────────────── Empty UA ──────────────
 * Per the plan, an empty UA returns NULL for browser/os/device.
 * ua_is_bot on empty is 0 (the empty string is not a bot). */
SELECT ua_browser('');
SELECT ua_os('');
SELECT ua_device('');
SELECT ua_is_bot('');

/* ────────────── Garbage UA ──────────────
 * Unparseable junk: scalars must return NULL (woothee returns None
 * from parse(), which we map to NULL); ua_is_bot must return 0. */
SELECT ua_browser('???not-a-real-ua???');
SELECT ua_is_bot('???not-a-real-ua???');

/* useragent_version() is non-empty. */
SELECT length(useragent_version()) > 0;
