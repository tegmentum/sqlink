//! `changeset` — SQLite session/changeset helpers (invert / concat / count /
//! tables / decode).
//!
//! THIN, GENERATED sqlink (`sqlite:extension`) shim: a
//! `wit_bindgen::generate!` block plus one `datalink_extcore::sqlite_shim!`
//! (the dynamically-loaded component path) and one
//! `datalink_extcore::embed_shim!` (the static embed path the CLI links in).
//! All logic + the capability surface live ONCE in datalink `changeset-core`,
//! a pure-Rust changeset codec. The registration ABI, func-id dispatch and the
//! `SqlValue` / `SqlValueOwned` marshalling are derived from the core's
//! `declare!` table.
//!
//! This replaces the previous embed-only `embed.rs` (which FFI'd into the
//! CLI's own sqlite3 session library). Because the codec is now pure Rust with
//! no C dependency, the extension builds as a standalone wasm provider.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed {
    datalink_extcore::embed_shim! {
        core = changeset_core::Core;
        sqlite_embed = sqlite_embed;
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-wit/wit/sqlite-extension",
            world: "minimal",
            generate_all,
        });
    }

    datalink_extcore::sqlite_shim! {
        core = changeset_core::Core;
        bindings = bindings;
        types = bindings::sqlite::extension::types;
        metadata = bindings::exports::sqlite::extension::metadata;
        scalar_function = bindings::exports::sqlite::extension::scalar_function;
        prefix_expansion = "com.tegmentum.sqlink.ext.changeset";
    }
}
