//! `isin` — THIN, GENERATED sqlink (`sqlite:extension`) shim.
//!
//! A `wit_bindgen::generate!` block plus one
//! `datalink_extcore::sqlite_shim!` (the dynamically-loaded component
//! path) and one `datalink_extcore::embed_shim!` (the static embed path
//! the CLI / host link in). All logic + the capability surface live ONCE
//! in datalink `isin-core`; the registration ABI, func-id dispatch, and
//! the `SqlValue` / `SqlValueOwned` marshalling are derived from the
//! core's `declare!` table. Replaces the previously hand-maintained
//! `lib.rs` + `embed.rs` copies (write-once dedup).

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed {
    datalink_extcore::embed_shim! {
        core = isin_core::Core;
        sqlite_embed = sqlite_embed;
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    datalink_extcore::sqlite_shim! {
        core = isin_core::Core;
        bindings = bindings;
        types = bindings::sqlite::extension::types;
        metadata = bindings::exports::sqlite::extension::metadata;
        scalar_function = bindings::exports::sqlite::extension::scalar_function;
        prefix_expansion = "com.tegmentum.sqlink.ext.isin";
    }
}
