//! `duckdbcompat` — DuckDB-native scalars SQLite lacks, for SQLite.
//!
//! THIN, GENERATED sqlink (`sqlite:extension`) shim: a
//! `wit_bindgen::generate!` block plus one `datalink_extcore::sqlite_shim!`
//! (the dynamically-loaded component path) and one
//! `datalink_extcore::embed_shim!` (the static embed path). All logic +
//! the capability surface live ONCE in datalink `duckdbcompat-core`.
//!
//! This is the cross-compat (#153) SQLite <- DuckDB direction: the names
//! are DuckDB builtins SQLite does not ship (`bar`, `even`, `gamma`,
//! `lgamma`, `nextafter`), so loading this extension gives a SQLite user
//! the DuckDB spellings + semantics. There is no ducklink counterpart
//! (DuckDB already has these as builtins). The already-pulled-up sqlink
//! packs (#152: `bit_count`, `gamma_cdf`/`gamma_pdf`, regexp/list/stdsql)
//! are NOT duplicated here.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed {
    datalink_extcore::embed_shim! {
        core = duckdbcompat_core::Core;
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
        core = duckdbcompat_core::Core;
        bindings = bindings;
        types = bindings::sqlite::extension::types;
        metadata = bindings::exports::sqlite::extension::metadata;
        scalar_function = bindings::exports::sqlite::extension::scalar_function;
        prefix_expansion = "com.tegmentum.sqlink.ext.duckdbcompat";
    }
}
