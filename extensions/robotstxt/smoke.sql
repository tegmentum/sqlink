.load extensions/robotstxt/target/wasm32-wasip2/release/robotstxt_extension.component.wasm

/* ---- robots_is_allowed: RFC 9309-style basic deny ----
 * Body:
 *   User-agent: FooBot
 *   Disallow: /
 * Expectation: every URL is disallowed for FooBot. */
SELECT robots_is_allowed(
  'User-agent: FooBot' || char(10) || 'Disallow: /' || char(10),
  'FooBot',
  'https://example.com/anything');

/* A different UA falls through (no '*' group), so it's allowed.
 * This is the Google parser's "ever_seen_specific_agent" semantic --
 * if no group matches you, the default is allow. */
SELECT robots_is_allowed(
  'User-agent: FooBot' || char(10) || 'Disallow: /' || char(10),
  'BarBot',
  'https://example.com/anything');

/* ---- '*' fallback group ----
 *   User-agent: *
 *   Disallow: /private/
 * Any UA without a specific group inherits these rules. */
SELECT robots_is_allowed(
  'User-agent: *' || char(10) || 'Disallow: /private/' || char(10),
  'GenericBot',
  'https://example.com/private/secret.html');

SELECT robots_is_allowed(
  'User-agent: *' || char(10) || 'Disallow: /private/' || char(10),
  'GenericBot',
  'https://example.com/public/index.html');

/* ---- per-UA specific overrides '*' ----
 *   User-agent: *
 *   Disallow: /
 *   User-agent: Googlebot
 *   Allow: /
 * Specific group wins (Googlebot allowed); '*' group denies others. */
SELECT robots_is_allowed(
  'User-agent: *' || char(10) || 'Disallow: /' || char(10) ||
  'User-agent: Googlebot' || char(10) || 'Allow: /' || char(10),
  'Googlebot',
  'https://example.com/x');

SELECT robots_is_allowed(
  'User-agent: *' || char(10) || 'Disallow: /' || char(10) ||
  'User-agent: Googlebot' || char(10) || 'Allow: /' || char(10),
  'OtherBot',
  'https://example.com/x');

/* ---- Allow-over-Disallow longest-match ----
 *   User-agent: *
 *   Disallow: /private/
 *   Allow: /private/public/
 * /private/public/foo.html is more specifically allowed. */
SELECT robots_is_allowed(
  'User-agent: *' || char(10) ||
  'Disallow: /private/' || char(10) ||
  'Allow: /private/public/' || char(10),
  'AnyBot',
  'https://example.com/private/public/foo.html');

SELECT robots_is_allowed(
  'User-agent: *' || char(10) ||
  'Disallow: /private/' || char(10) ||
  'Allow: /private/public/' || char(10),
  'AnyBot',
  'https://example.com/private/secret.html');

/* ---- empty robots body -> all allowed ---- */
SELECT robots_is_allowed('', 'AnyBot', 'https://example.com/anything');

/* ---- robots_crawl_delay ----
 * Bing-style:
 *   User-agent: *
 *   Crawl-delay: 5
 * Any UA picks up 5.0 seconds. */
SELECT robots_crawl_delay(
  'User-agent: *' || char(10) || 'Crawl-delay: 5' || char(10),
  'AnyBot');

/* UA-specific overrides '*':
 *   User-agent: *
 *   Crawl-delay: 10
 *   User-agent: Bingbot
 *   Crawl-delay: 1
 * Bingbot -> 1.0; others -> 10.0. */
SELECT robots_crawl_delay(
  'User-agent: *' || char(10) || 'Crawl-delay: 10' || char(10) ||
  'User-agent: Bingbot' || char(10) || 'Crawl-delay: 1' || char(10),
  'Bingbot');

SELECT robots_crawl_delay(
  'User-agent: *' || char(10) || 'Crawl-delay: 10' || char(10) ||
  'User-agent: Bingbot' || char(10) || 'Crawl-delay: 1' || char(10),
  'GenericBot');

/* Crawl-delay absent -> NULL (no record applies). */
SELECT robots_crawl_delay(
  'User-agent: *' || char(10) || 'Disallow: /private/' || char(10),
  'AnyBot');

/* Fractional crawl-delay: "0.5" parses as a real. */
SELECT robots_crawl_delay(
  'User-agent: *' || char(10) || 'Crawl-delay: 0.5' || char(10),
  'AnyBot');

/* ---- robots_sitemaps ----
 * Single sitemap at top of file. */
SELECT robots_sitemaps(
  'Sitemap: https://example.com/sitemap.xml' || char(10) ||
  'User-agent: *' || char(10) || 'Disallow: /private/' || char(10));

/* Multiple sitemaps, order preserved. */
SELECT robots_sitemaps(
  'Sitemap: https://example.com/sitemap-1.xml' || char(10) ||
  'Sitemap: https://example.com/sitemap-2.xml' || char(10) ||
  'User-agent: *' || char(10) || 'Disallow:' || char(10));

/* No sitemaps -> empty JSON array. */
SELECT robots_sitemaps(
  'User-agent: *' || char(10) || 'Disallow: /' || char(10));

/* Empty robots body -> empty JSON array. */
SELECT robots_sitemaps('');

/* ---- NULL propagation ---- */
SELECT robots_is_allowed(NULL, 'Bot', 'https://example.com/');
SELECT robots_is_allowed('User-agent: *' || char(10) || 'Disallow: /', NULL, 'https://example.com/');
SELECT robots_is_allowed('User-agent: *' || char(10) || 'Disallow: /', 'Bot', NULL);
SELECT robots_crawl_delay(NULL, 'Bot');
SELECT robots_crawl_delay('User-agent: *', NULL);
SELECT robots_sitemaps(NULL);

/* ---- version is non-empty ---- */
SELECT length(robotstxt_version()) > 0;
