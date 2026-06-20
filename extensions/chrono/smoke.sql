.load extensions/chrono/target/wasm32-wasip2/release/chrono_extension.component.wasm

/* ── ISO 8601 parse round-trip ── */
SELECT date_parse('2025-06-20T15:30:00Z');

/* ── strftime format ── */
SELECT date_format('2025-06-20T15:30:00Z', '%Y/%m/%d');

/* ── add days ── */
SELECT date_add('2025-06-20', 5, 'days');

/* ── add months (calendar-aware) ── */
SELECT date_add('2025-01-31', 1, 'months');

/* ── diff days (a - b) ── */
SELECT date_diff('2025-06-25', '2025-06-20', 'days');

/* ── tz convert UTC → America/New_York at 12:00 UTC in June (EDT, UTC-4)
 *    → 08:00 local. */
SELECT date_tz_convert('2025-06-20T12:00:00Z', 'UTC', 'America/New_York');

/* ── ISO 8601 week + year ── */
SELECT date_iso_week('2024-01-01');
SELECT date_iso_year('2024-12-30');

/* ── business-day flag ── */
SELECT date_is_business_day('2025-06-21');     -- Saturday → 0
SELECT date_is_business_day('2025-06-20');     -- Friday   → 1

/* ── business-day count ── */
SELECT date_business_days_between('2024-01-01', '2024-01-08');

/* ── duration parse forms ── */
SELECT duration_parse('1d 3h');
SELECT duration_parse('90');
SELECT duration_parse('PT1H30M');
SELECT duration_parse('1.5h');

/* ── duration format ── */
SELECT duration_format(3600);
SELECT duration_format(97200);
SELECT duration_format(0);
SELECT duration_format(-3600);
SELECT duration_format(90061, 2);              -- precision-capped

/* ── version is non-empty ── */
SELECT length(chrono_version()) > 0;
