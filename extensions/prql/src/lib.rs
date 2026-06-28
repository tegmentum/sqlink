//! `prql` TRANSPARENT parser extension — sqlink (`sqlite:extension`) port.
//!
//! THIN, GENERATED shim: a `wit_bindgen::generate!` block plus one
//! `datalink_extcore::sqlite_shim!`. All logic + the capability surface
//! live ONCE in datalink `prql-core` (wraps prqlc, dialect-parameterized);
//! the manifest, func-id dispatch, and the `SqlValue` marshalling are
//! derived from the core's `declare!` table — the same scalar codegen the
//! rest of the catalog uses.
//!
//! # How a parser extension works on SQLite
//!
//! Unlike DuckDB (a pluggable `ParserExtension`), SQLite's amalgamation
//! parser cannot be extended, so there is no in-engine hook. The host
//! shell instead INTERCEPTS statements the built-in parser rejected and
//! offers them to the reserved scalar entrypoint `__sqlink_parse` that
//! `prql-core` declares. A bare `from x | filter .. | select ..` PRQL
//! pipeline is compiled to the SQLite dialect and run in place — the
//! transparent upgrade of the explicit `prql_to_sql` scalar. This
//! component loads as a plain `minimal`-world scalar extension; the parser
//! capability is realized entirely by the host-shell intercept + the
//! entrypoint convention.

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
        core = prql_core::Core;
        bindings = bindings;
        types = bindings::sqlite::extension::types;
        metadata = bindings::exports::sqlite::extension::metadata;
        scalar_function = bindings::exports::sqlite::extension::scalar_function;
        prefix_expansion = "com.tegmentum.sqlink.ext.prql";
    }
}
