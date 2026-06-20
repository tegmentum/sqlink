-- Seed the plugin_candidate survey.
--
-- Items here have been identified during planning sessions or
-- ecosystem audits as worth porting but haven't shipped yet.
-- When a candidate graduates (i.e. an extensions/<name>/ dir
-- lands), DELETE its row.
--
-- Sources:
--   sqlean             https://github.com/nalgeon/sqlean
--   sqlite-upstream    bundled with SQLite (json2, btreeinfo, etc.)
--   session-2026-06    items mentioned across the round 1-5 plans
--                      but not yet shipped
--   ecosystem          known third-party extensions / crate-based
--                      candidates

BEGIN;

DELETE FROM plugin_candidate;

-- ====== Crypto + wire-format gaps ======
INSERT INTO plugin_candidate (name, source, description, upstream_url, track, proposed_crate, added_at, notes)
VALUES
('hpke',             'session-2026-06', 'Hybrid Public Key Encryption (RFC 9180).',
                     'https://datatracker.ietf.org/doc/html/rfc9180',
                     'crypto', 'hpke 0.13', unixepoch(),
                     'Modern asymmetric encryption suite; pairs with aead + hkdf.'),
('cose',             'session-2026-06', 'CBOR Object Signing and Encryption (RFC 8152).',
                     'https://datatracker.ietf.org/doc/html/rfc8152',
                     'crypto', 'coset 0.3', unixepoch(),
                     'CBOR-shaped JOSE alternative; used by WebAuthn / FIDO2.'),
('webauthn',         'session-2026-06', 'WebAuthn registration + authentication verification.',
                     'https://www.w3.org/TR/webauthn-2/',
                     'crypto', 'webauthn-rs 0.5', unixepoch(),
                     'Heavy spec; needs handler-side surface as well as SQL.'),
('pgp',              'session-2026-06', 'OpenPGP key parsing + sign/verify (RFC 4880).',
                     'https://datatracker.ietf.org/doc/html/rfc4880',
                     'crypto', 'pgp 0.13', unixepoch(),
                     'sequoia-openpgp is too heavy; pgp crate is the lighter choice.'),

-- ====== Codec gaps ======
('protobuf',         'session-2026-06', 'Protocol Buffers encode/decode (given schema).',
                     'https://protobuf.dev/programming-guides/encoding/',
                     'codec', 'prost 0.13', unixepoch(),
                     'Schema interface is the hard part; flatbuffers + thrift have the same shape.'),
('flatbuffers',      'session-2026-06', 'FlatBuffers binary codec (Google).',
                     'https://flatbuffers.dev/',
                     'codec', 'flatbuffers 24', unixepoch(),
                     'Same schema problem as protobuf.'),
('thrift',           'session-2026-06', 'Apache Thrift binary codec.',
                     'https://thrift.apache.org/',
                     'codec', 'thrift 0.17', unixepoch(),
                     'Schema problem; less common than protobuf.'),
('yaml',             'session-2026-06', 'YAML parse/serialize as scalars (yaml-to-json handler exists; this is SQL-side).',
                     'https://yaml.org/spec/1.2.2/',
                     'codec', 'serde_yaml 0.9', unixepoch(),
                     'handlers/yaml-to-json covers the httpd side; SQL scalar form is missing.'),

-- ====== Document / media ======
('epub',             'session-2026-06', 'EPUB e-book metadata extraction.',
                     'https://www.w3.org/publishing/epub32/',
                     'media', 'epub 2 (or rbook 0.9)', unixepoch(),
                     'image-meta + pdf-meta + id3 + vcard cover other formats; epub is the next.'),
('djvu',             'session-2026-06', 'DjVu document metadata.',
                     'http://djvu.org/',
                     'media', 'no crate yet  port djvulibre header', unixepoch(),
                     'Niche; defer until a consumer asks.'),
('docx-meta',        'session-2026-06', 'OOXML (docx/xlsx/pptx) metadata extraction.',
                     'https://www.ecma-international.org/publications-and-standards/standards/ecma-376/',
                     'media', 'docx-rs 0.4', unixepoch(),
                     'OOXML is a zip + XML; might overlap with formats/xml + zipfile.'),
('perceptual-hash',  'session-2026-06', 'pHash / dHash / aHash for image similarity.',
                     'https://en.wikipedia.org/wiki/Perceptual_hashing',
                     'media', 'img_hash 3', unixepoch(),
                     'Needs the image crate for pixel decode  bigger wasm bundle than image-meta.'),
('color-palette',    'session-2026-06', 'Extract dominant color palette from image blobs.',
                     'https://en.wikipedia.org/wiki/Color_quantization',
                     'media', 'kmeans-colors 0.6', unixepoch(),
                     'Pairs with color + image-meta.'),

-- ====== Geo coordinate systems ======
('proj',             'session-2026-06', 'PROJ-style coordinate reference system transformations.',
                     'https://proj.org/',
                     'geo', 'proj 0.27 (C dep)', unixepoch(),
                     'Heavy C dependency; defer until needed.'),

-- ====== Text / NLP ======
('lemmatize',        'session-2026-06', 'Dictionary-based lemmatization (vs stemmers morphological reduction).',
                     'https://en.wikipedia.org/wiki/Lemmatisation',
                     'text', 'lemmatizer 0.1', unixepoch(),
                     'stemmer (Snowball) is morphological; lemma uses a lookup table.'),
('sentence-split',   'session-2026-06', 'Sentence boundary detection for arbitrary text.',
                     'https://en.wikipedia.org/wiki/Sentence_boundary_disambiguation',
                     'text', 'pragmatic-segmenter 0.2', unixepoch(),
                     'Common preprocessing step; non-trivial for non-English.'),
('pinyin',           'session-2026-06', 'Chinese pinyin transliteration.',
                     'https://en.wikipedia.org/wiki/Pinyin',
                     'text', 'pinyin 0.10', unixepoch(),
                     'Mainland-China-specific; useful for CN-language pipelines.'),

-- ====== Network / web ======
('http-signature',   'session-2026-06', 'HTTP Message Signatures (RFC 9421).',
                     'https://datatracker.ietf.org/doc/html/rfc9421',
                     'network', 'no canonical crate yet  hand-roll', unixepoch(),
                     'Used by ActivityPub / Mastodon and the SSF working group.'),
('whois-parse',      'session-2026-06', 'Parse WHOIS response text (not lookup).',
                     'https://datatracker.ietf.org/doc/html/rfc3912',
                     'network', 'whois-rust 1', unixepoch(),
                     'Many response formats; pick the common ARIN/RIPE shapes.'),
('sitemap-xml',      'session-2026-06', 'sitemap.xml + sitemap-index parsing.',
                     'https://www.sitemaps.org/protocol.html',
                     'network', 'sitemap 0.4', unixepoch(),
                     'Pairs with robotstxt for crawler use cases.'),
('tld-list',         'session-2026-06', 'Comprehensive TLD list with metadata (gtld vs cctld, etc).',
                     'https://www.iana.org/domains/root/db',
                     'network', 'iana-tld 0.3', unixepoch(),
                     'publicsuffix covers the PSL; this is the IANA gold-source root zone.'),

-- ====== Bibliographic / identifiers ======
('iso-639-5',        'session-2026-06', 'ISO 639-5 (language families) on top of existing iso-codes.',
                     'https://www.iso.org/standard/39536.html',
                     'bibliographic', 'iso_639 0.4', unixepoch(),
                     'iso-codes covers 639-1/639-2/639-3; this is the family bucket.'),
('vat',              'session-2026-06', 'VAT number validation (EU + common non-EU formats).',
                     'https://ec.europa.eu/taxation_customs/vies/',
                     'bibliographic', 'vatable 0.2 OR roll-own', unixepoch(),
                     'Per-country checksum rules; tedious but well-defined.'),
('lei',              'session-2026-06', 'Legal Entity Identifier validation (ISO 17442).',
                     'https://www.iso.org/standard/75998.html',
                     'bibliographic', 'no crate yet  hand-roll', unixepoch(),
                     '20 alphanumeric chars + mod-97 check; small.'),

-- ====== Data structures ======
('skiplist',         'session-2026-06', 'Skip list as a vtab.',
                     'https://en.wikipedia.org/wiki/Skip_list',
                     'data-structures', 'skiplist 0.5', unixepoch(),
                     'roaring covers exact set; skiplist is sorted-set.'),

-- ====== Math / scientific ======
('rsa-bignum',       'session-2026-06', 'Standalone RSA-style bignum modexp + key gen (separate from `rsa` crate).',
                     'https://datatracker.ietf.org/doc/html/rfc8017',
                     'math', 'num-bigint 0.4 + custom', unixepoch(),
                     'bignum has modpow; this would add a key-gen surface specifically.'),
('signal-processing','session-2026-06', 'FIR/IIR filters, autocorrelation, convolution beyond fft.',
                     'https://en.wikipedia.org/wiki/Digital_signal_processing',
                     'math', 'biquad 0.4 + dsp 0.1', unixepoch(),
                     'fft covers transform; this is the filtering surface.'),

-- ====== Sqlean items not covered by our catalog ======
('sqlean-vsv',       'sqlean',          'Virtual CSV view (vsv).',
                     'https://github.com/nalgeon/sqlean/blob/main/docs/vsv.md',
                     'vtab', 'port from sqlean C', unixepoch(),
                     'csv extension exists but vsv has different shape  worth comparing.'),

-- ====== Specialty / niche ======
('mqtt-parse',       'session-2026-06', 'MQTT message parsing (control packets).',
                     'https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html',
                     'network', 'mqtt-codec 0.7', unixepoch(),
                     'IoT message parsing.'),
('nmea',             'session-2026-06', 'NMEA 0183 GPS sentence parsing.',
                     'https://www.nmea.org/Assets/0183-2015-1-1.pdf',
                     'network', 'nmea 0.6', unixepoch(),
                     'GPS receiver output parsing.'),
('mbox',             'session-2026-06', 'mbox + maildir email format parsing as vtab.',
                     'https://datatracker.ietf.org/doc/html/rfc4155',
                     'vtab', 'mbox-reader 0.2', unixepoch(),
                     'Useful for email-database use cases.'),
('tar',              'session-2026-06', 'tar archive parsing as vtab.',
                     'https://www.gnu.org/software/tar/manual/html_node/Standard.html',
                     'codec', 'tar 0.4', unixepoch(),
                     'zipfile exists; tar is the unix analog.'),
('dicom',            'session-2026-06', 'DICOM medical imaging metadata.',
                     'https://www.dicomstandard.org/',
                     'media', 'dicom-rs 0.7', unixepoch(),
                     'Medical / healthcare context.'),
('wasm-introspect',  'session-2026-06', 'wasm module introspection (custom sections, imports/exports).',
                     'https://webassembly.github.io/spec/core/',
                     'codec', 'wasmparser 0.215', unixepoch(),
                     'Meta: query the catalog''s own components via SQL.'),
('mathml',           'session-2026-06', 'MathML parsing.',
                     'https://www.w3.org/Math/',
                     'document', 'mathml-rs 0.1', unixepoch(),
                     'Niche; specialized for scientific document workflows.');

COMMIT;

SELECT 'seeded ' || COUNT(*) || ' candidates' FROM plugin_candidate;
SELECT track, COUNT(*) FROM plugin_candidate GROUP BY track ORDER BY track;
