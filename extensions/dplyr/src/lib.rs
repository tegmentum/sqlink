//! `dplyr` parser extension — sqlink (`sqlite:extension`) port.
//!
//! THIN, GENERATED shim: a `wit_bindgen::generate!` block plus one
//! `datalink_extcore::sqlite_shim!`. All logic + the capability surface
//! live ONCE in datalink `dplyr-core`; the manifest, func-id dispatch,
//! and the `SqlValue` marshalling are derived from the core's `declare!`
//! table — the same scalar codegen the rest of the catalog uses.
//!
//! # How a parser extension works on SQLite
//!
//! Unlike DuckDB (a pluggable `ParserExtension`), SQLite's amalgamation
//! parser cannot be extended, so there is no in-engine hook. The host
//! shell instead INTERCEPTS statements the built-in parser rejected and
//! offers them to the reserved scalar entrypoint `__sqlink_parse` that
//! `dplyr-core` declares. This component therefore loads as a plain
//! `minimal`-world scalar extension: the parser capability is realized
//! entirely by the host-shell intercept + the entrypoint convention, so
//! no new world / bindgen is needed.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    datalink_extcore::sqlite_shim! {
        core = dplyr_core::Core;
        bindings = bindings;
        types = bindings::sqlite::extension::types;
        metadata = bindings::exports::sqlite::extension::metadata;
        scalar_function = bindings::exports::sqlite::extension::scalar_function;
        prefix_expansion = "com.tegmentum.sqlink.ext.dplyr";
    }
}
