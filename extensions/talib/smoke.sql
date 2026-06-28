-- talib smoke: TA-Lib indicators as SQLite WINDOW functions over a frame.
-- Each aggregate is advertised is-window=true, so the loader registers it
-- via create_window_function and SQLite drives step/inverse/value/finalize.
.load extensions/talib/target/wasm32-wasip2/release/talib_extension.component.wasm

CREATE TABLE prices(t INTEGER, close REAL);
INSERT INTO prices VALUES (1,10),(2,11),(3,12),(4,13),(5,14),(6,13),(7,11);

-- Plain-aggregate form (no OVER) over the whole table.
SELECT round(sma(close), 4) FROM prices;

-- 3-period windowed SMA / EMA / RSI (ROWS BETWEEN 2 PRECEDING AND CURRENT).
SELECT t,
  round(sma(close) OVER w, 4) AS sma3,
  round(ema(close) OVER w, 4) AS ema3,
  round(rsi(close) OVER w, 4) AS rsi3
FROM prices
WINDOW w AS (ORDER BY t ROWS BETWEEN 2 PRECEDING AND CURRENT ROW)
ORDER BY t;
