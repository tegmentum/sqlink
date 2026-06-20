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

-- ====== Bibliographic / identifiers ======

-- ====== Data structures ======

-- ====== Math / scientific ======
('rsa-bignum',       'session-2026-06', 'Standalone RSA-style bignum modexp + key gen (separate from `rsa` crate).',
                     'https://datatracker.ietf.org/doc/html/rfc8017',
                     'math', 'num-bigint 0.4 + custom', unixepoch(),
                     'bignum has modpow; this would add a key-gen surface specifically.'),

-- ====== Sqlean items not covered by our catalog ======

-- ====== Specialty / niche ======
('mathml',           'session-2026-06', 'MathML parsing.',
                     'https://www.w3.org/Math/',
                     'document', 'mathml-rs 0.1', unixepoch(),
                     'Niche; specialized for scientific document workflows.');

-- ====== Survey decisions (2026-06-20) ======
-- Items the user explicitly declined to pursue, with reasons.
-- Future sessions can clear status='blocked' + reason if the
-- ecosystem changes or a consumer surfaces.

UPDATE plugin_candidate SET status = 'deferred',
       reason = 'Schema-driven codec; SQL surface design requires a real consumer to pin the right shape (per-message generated table? generic JSON pivot? schema-as-SQL-blob?). Defer until ask.'
 WHERE name IN ('protobuf', 'flatbuffers', 'thrift');

UPDATE plugin_candidate SET status = 'blocked',
       reason = 'proj 0.27 wraps the PROJ C library. PROJ does not cross-compile cleanly to wasm32-wasip2 via the wasi-sdk clang  defer until a pure-rust reprojection library covers the workflow.'
 WHERE name = 'proj';

UPDATE plugin_candidate SET status = 'blocked',
       reason = 'No actively-maintained pure-rust DjVu crate. The historical option is to port djvulibre header parsing  out of scope without a consumer.'
 WHERE name = 'djvu';

UPDATE plugin_candidate SET status = 'blocked',
       reason = 'mathml-rs 0.1 does not exist on crates.io. MathML parsing via roll-our-own quick-xml is doable but niche  defer until a consumer asks.'
 WHERE name = 'mathml';

UPDATE plugin_candidate SET status = 'skipped',
       reason = 'Defunct: the rsa extension (rsa 0.9 via num-bigint internally) shipped in round 2, covering key generation + sign/verify/encrypt/decrypt. A separate bignum-backed implementation no longer adds capability.'
 WHERE name = 'rsa-bignum';

COMMIT;

SELECT 'seeded ' || COUNT(*) || ' candidates' FROM plugin_candidate;
SELECT track, status, COUNT(*) FROM plugin_candidate GROUP BY track, status ORDER BY track, status;
