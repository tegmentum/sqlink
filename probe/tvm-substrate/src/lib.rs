//! TVM substrate probe. Same shape as
//! `~/git/tvm-wasm/examples/guest-demo`, but built for
//! wasm32-wasip2 + component-model instead of
//! wasm32-unknown-unknown + raw module.
//!
//! Validates that the SQLite track's Phase 1 (Path B pcache2) can
//! plug into the existing tvm-wasmtime add_to_linker plumbing.

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "tvm-guest-demo",
        generate_all,
    });
}

use bindings::tvm::memory::bytes;
use bindings::tvm::memory::manager;
use bindings::tvm::memory::types::RegionKind;

struct Probe;

impl bindings::Guest for Probe {
    fn run_test() -> u32 {
        let region = manager::create_region(RegionKind::HotHeap, 256)
            .expect("create-region");
        let h = manager::alloc(region, 4).expect("alloc");
        bytes::write(h, &[1, 2, 3, 4]).expect("write");
        let read = bytes::read(h, 4).expect("read");
        read.iter().map(|b| *b as u32).sum()
    }
}

bindings::export!(Probe with_types_in bindings);
