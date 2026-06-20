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
('webauthn',         'session-2026-06', 'WebAuthn registration + authentication verification.',
                     'https://www.w3.org/TR/webauthn-2/',
                     'crypto', 'webauthn-rs 0.5', unixepoch(),
                     'Heavy spec; needs handler-side surface as well as SQL.'),

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

-- ====== Document / media ======
('djvu',             'session-2026-06', 'DjVu document metadata.',
                     'http://djvu.org/',
                     'media', 'no crate yet  port djvulibre header', unixepoch(),
                     'Niche; defer until a consumer asks.'),

-- ====== Geo coordinate systems ======
('proj',             'session-2026-06', 'PROJ-style coordinate reference system transformations.',
                     'https://proj.org/',
                     'geo', 'proj 0.27 (C dep)', unixepoch(),
                     'Heavy C dependency; defer until needed.'),

-- ====== Text / NLP ======

-- ====== Network / web ======
('http-signature',   'session-2026-06', 'HTTP Message Signatures (RFC 9421).',
                     'https://datatracker.ietf.org/doc/html/rfc9421',
                     'network', 'no canonical crate yet  hand-roll', unixepoch(),
                     'Used by ActivityPub / Mastodon and the SSF working group.'),

-- ====== Bibliographic / identifiers ======

-- ====== Data structures ======

-- ====== Math / scientific ======
('rsa-bignum',       'session-2026-06', 'Standalone RSA-style bignum modexp + key gen (separate from `rsa` crate).',
                     'https://datatracker.ietf.org/doc/html/rfc8017',
                     'math', 'num-bigint 0.4 + custom', unixepoch(),
                     'bignum has modpow; this would add a key-gen surface specifically.'),

-- ====== Sqlean items not covered by our catalog ======
('sqlean-vsv',       'sqlean',          'Virtual CSV view (vsv).',
                     'https://github.com/nalgeon/sqlean/blob/main/docs/vsv.md',
                     'vtab', 'port from sqlean C', unixepoch(),
                     'csv extension exists but vsv has different shape  worth comparing.'),

-- ====== Specialty / niche ======
('dicom',            'session-2026-06', 'DICOM medical imaging metadata.',
                     'https://www.dicomstandard.org/',
                     'media', 'dicom-rs 0.7', unixepoch(),
                     'Medical / healthcare context.'),
('mathml',           'session-2026-06', 'MathML parsing.',
                     'https://www.w3.org/Math/',
                     'document', 'mathml-rs 0.1', unixepoch(),
                     'Niche; specialized for scientific document workflows.');

COMMIT;

SELECT 'seeded ' || COUNT(*) || ' candidates' FROM plugin_candidate;
SELECT track, COUNT(*) FROM plugin_candidate GROUP BY track ORDER BY track;
