//! `talib` — TA-Lib style technical indicators as SQLite WINDOW
//! functions.
//!
//! THIN, GENERATED sqlink (`sqlite:extension`) shim: a
//! `wit_bindgen::generate!` block (the `stateful` world, which exports
//! `aggregate-function` = step/finalize/value/inverse) plus one
//! `datalink_extcore::sqlite_agg_shim!`. All logic + the capability
//! surface (the `sma` / `ema` / `rsi` folds over a frame) live ONCE in
//! datalink `talib-core`; the manifest, func-id dispatch, the per-context
//! frame buffer, and the `SqlValue` marshalling are derived from the
//! core's `declare!` table.
//!
//! Each aggregate is advertised with `is-window: true`, so the sqlink
//! loader registers it through `create_window_function` and SQLite drives
//! it over an `OVER (... ROWS BETWEEN ...)` frame — the SAME `talib-core`
//! that rides DuckDB's `call-aggregate-window` path in ducklink.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "stateful",
            generate_all,
        });
    }

    datalink_extcore::sqlite_agg_shim! {
        core = talib_core::Core;
        bindings = bindings;
        types = bindings::sqlite::extension::types;
        metadata = bindings::exports::sqlite::extension::metadata;
        scalar_function = bindings::exports::sqlite::extension::scalar_function;
        aggregate_function = bindings::exports::sqlite::extension::aggregate_function;
        prefix_expansion = "com.tegmentum.sqlink.ext.talib";
    }
}
