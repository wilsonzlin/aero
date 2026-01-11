// wasm-specific render tests live in `tests/wasm/`.
//
// This integration test is a tiny shim so `cargo test --target wasm32-unknown-unknown`
// can pick them up (when driven by an external harness).

#[cfg(target_arch = "wasm32")]
mod wasm;
