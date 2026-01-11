// Include the shared test module without colliding with `tests/wasm.rs`, which
// is a wasm32-only shim.
mod common;

#[path = "wasm/mod.rs"]
mod wasm_tests;
