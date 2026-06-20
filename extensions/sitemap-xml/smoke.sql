.load extensions/sitemap-xml/target/wasm32-wasip2/release/sitemap_xml_extension.component.wasm

/* ---- sitemap_urls on a basic urlset ----
 * Single-line XML to dodge cli line continuation quirks. */
SELECT sitemap_urls(
  '<?xml version="1.0" encoding="UTF-8"?>' ||
  '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<url><loc>https://example.com/a</loc></url>' ||
    '<url><loc>https://example.com/b</loc></url>' ||
  '</urlset>');

/* ---- sitemap_full surfaces optional fields ---- */
SELECT sitemap_full(
  '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<url>' ||
      '<loc>https://example.com/a</loc>' ||
      '<lastmod>2024-01-15</lastmod>' ||
      '<changefreq>weekly</changefreq>' ||
      '<priority>0.8</priority>' ||
    '</url>' ||
  '</urlset>');

/* ---- sitemap_full leaves missing fields null ---- */
SELECT sitemap_full(
  '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<url><loc>https://example.com/c</loc></url>' ||
  '</urlset>');

/* ---- sitemap_index_locs on a sitemap-index document ---- */
SELECT sitemap_index_locs(
  '<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<sitemap><loc>https://example.com/sitemap-1.xml</loc></sitemap>' ||
    '<sitemap><loc>https://example.com/sitemap-2.xml</loc></sitemap>' ||
  '</sitemapindex>');

/* ---- cross-kind: urlset asked for index locs -> [] ---- */
SELECT sitemap_index_locs(
  '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<url><loc>https://example.com/a</loc></url>' ||
  '</urlset>');

/* ---- cross-kind: sitemap-index asked for urls -> [] ---- */
SELECT sitemap_urls(
  '<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<sitemap><loc>https://example.com/sitemap-1.xml</loc></sitemap>' ||
  '</sitemapindex>');

/* ---- sitemap_count on a urlset (3 records) ---- */
SELECT sitemap_count(
  '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<url><loc>https://example.com/a</loc></url>' ||
    '<url><loc>https://example.com/b</loc></url>' ||
    '<url><loc>https://example.com/c</loc></url>' ||
  '</urlset>');

/* ---- sitemap_count on a sitemap-index (2 records) ---- */
SELECT sitemap_count(
  '<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<sitemap><loc>https://example.com/sitemap-1.xml</loc></sitemap>' ||
    '<sitemap><loc>https://example.com/sitemap-2.xml</loc></sitemap>' ||
  '</sitemapindex>');

/* ---- empty urlset -> 0 ---- */
SELECT sitemap_count(
  '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"></urlset>');

/* ---- sitemap_is_valid: well-formed urlset -> 1 ---- */
SELECT sitemap_is_valid(
  '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<url><loc>https://example.com/</loc></url>' ||
  '</urlset>');

/* ---- sitemap_is_valid: well-formed sitemap-index -> 1 ---- */
SELECT sitemap_is_valid(
  '<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<sitemap><loc>https://example.com/s.xml</loc></sitemap>' ||
  '</sitemapindex>');

/* ---- sitemap_is_valid: wrong root (rss) -> 0 ---- */
SELECT sitemap_is_valid('<rss><channel/></rss>');

/* ---- sitemap_is_valid: junk -> 0 ---- */
SELECT sitemap_is_valid('not xml at all <<<<');

/* ---- sitemap_is_valid: empty -> 0 ---- */
SELECT sitemap_is_valid('');

/* ---- namespace-prefixed urlset still parses ---- */
SELECT sitemap_urls(
  '<sm:urlset xmlns:sm="http://www.sitemaps.org/schemas/sitemap/0.9">' ||
    '<sm:url><sm:loc>https://example.com/ns</sm:loc></sm:url>' ||
  '</sm:urlset>');

/* ---- NULL propagation ---- */
SELECT sitemap_urls(NULL);
SELECT sitemap_full(NULL);
SELECT sitemap_index_locs(NULL);
SELECT sitemap_count(NULL);
SELECT sitemap_is_valid(NULL);

/* ---- version string is non-empty ---- */
SELECT length(sitemap_version()) > 0;
