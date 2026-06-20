.load extensions/nmea/target/wasm32-wasip2/release/nmea_extension.component.wasm

/* ────────────── GGA (Global Positioning Fix Data) ──────────────
 * Acceptance: sentence_type=GGA, lat=53.36133..., lng=-6.50562...,
 * fix_quality=1, satellites=8, timestamp=09:27:50.
 * Sentence from gpsd; checksum validated upstream. */
SELECT nmea_sentence_type('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76');
SELECT round(nmea_lat('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76'), 5);
SELECT round(nmea_lng('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76'), 5);
SELECT nmea_fix_quality('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76');
SELECT nmea_satellites('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76');
SELECT nmea_timestamp('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76');
SELECT nmea_checksum_ok('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76');

/* ────────────── RMC (Recommended Minimum) ──────────────
 * Acceptance: sentence_type=RMC, lat=49.27416, lng=-123.18533,
 * speed=0.5 knots, course=54.7, timestamp carries the date. */
SELECT nmea_sentence_type('$GPRMC,225446.33,A,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E,A*2B');
SELECT round(nmea_lat('$GPRMC,225446.33,A,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E,A*2B'), 5);
SELECT round(nmea_lng('$GPRMC,225446.33,A,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E,A*2B'), 5);
SELECT round(nmea_speed_knots('$GPRMC,225446.33,A,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E,A*2B'), 2);
SELECT round(nmea_course('$GPRMC,225446.33,A,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E,A*2B'), 2);
SELECT nmea_timestamp('$GPRMC,225446.33,A,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E,A*2B');
SELECT nmea_checksum_ok('$GPRMC,225446.33,A,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E,A*2B');

/* ────────────── VTG (Track + Speed) ──────────────
 * Acceptance: speed_knots=5.5, course=54.7. The kph field (010.2,K)
 * is ignored when N-knots is present. */
SELECT nmea_sentence_type('$GPVTG,054.7,T,034.4,M,005.5,N,010.2,K*48');
SELECT round(nmea_speed_knots('$GPVTG,054.7,T,034.4,M,005.5,N,010.2,K*48'), 2);
SELECT round(nmea_course('$GPVTG,054.7,T,034.4,M,005.5,N,010.2,K*48'), 2);
SELECT nmea_checksum_ok('$GPVTG,054.7,T,034.4,M,005.5,N,010.2,K*48');

/* ────────────── GLL (Geographic Position) ──────────────
 * Acceptance: sentence_type=GLL; lat/lng pulled from this sentence
 * even though GLL has no fix-quality or speed columns. */
SELECT nmea_sentence_type('$GPGLL,4916.45,N,12311.12,W,225444,A,A*5C');
SELECT round(nmea_lat('$GPGLL,4916.45,N,12311.12,W,225444,A,A*5C'), 5);
SELECT round(nmea_lng('$GPGLL,4916.45,N,12311.12,W,225444,A,A*5C'), 5);
SELECT nmea_timestamp('$GPGLL,4916.45,N,12311.12,W,225444,A,A*5C');

/* ────────────── nmea_parse JSON dump ──────────────
 * Acceptance: every field is present and typed. Round-trip via
 * json_extract validates the schema. */
SELECT json_extract(nmea_parse('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76'), '$.sentence_type');
SELECT json_extract(nmea_parse('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76'), '$.fix_quality');
SELECT json_extract(nmea_parse('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76'), '$.satellites');
SELECT json_extract(nmea_parse('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76'), '$.checksum_ok');
SELECT json_extract(nmea_parse('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76'), '$.talker_id');

/* ────────────── Checksum failure path ──────────────
 * Acceptance: nmea_checksum_ok returns 0 (NOT NULL) for a corrupted
 * sentence; the descriptive scalars return NULL because parse_str
 * refuses to parse the body when the checksum mismatches.
 * Original 92750 GGA has checksum *76; we flip to *77. */
SELECT nmea_checksum_ok('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*77');
SELECT nmea_lat('$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*77');

/* ────────────── Garbage / non-NMEA input ──────────────
 * Acceptance: every scalar returns NULL except nmea_checksum_ok
 * which returns 0. */
SELECT nmea_sentence_type('not nmea at all');
SELECT nmea_lat('not nmea at all');
SELECT nmea_checksum_ok('not nmea at all');

/* ────────────── Unsupported sentence type ──────────────
 * Acceptance: HDT carries no lat/lng/speed; sentence_type still
 * resolves to "HDT" because the header parser doesn't care which
 * variant the body belongs to. */
SELECT nmea_sentence_type('$GPHDT,123.456,T*00');
SELECT nmea_lat('$GPHDT,123.456,T*00');

/* ────────────── NULL passthrough ──────────────
 * Every parser scalar maps NULL  NULL (the harness sets
 * `.nullvalue <NULL>` so a NULL result prints `<NULL>`). */
SELECT nmea_sentence_type(NULL);
SELECT nmea_lat(NULL);
SELECT nmea_checksum_ok(NULL);
SELECT nmea_parse(NULL);

/* nmea_version() is non-empty. */
SELECT length(nmea_version()) > 0;
