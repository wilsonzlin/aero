#![cfg_attr(feature = "wasm-threaded", feature(thread_local))]

use wasm_bindgen::prelude::*;

// wasm-bindgen's "threads" transform expects TLS metadata symbols (e.g.
// `__tls_size`) to exist in shared-memory builds. Those symbols are only emitted
// by the linker when there is at least one TLS variable. We keep a tiny TLS
// slot behind a cargo feature enabled only for the threaded build.
#[cfg(feature = "wasm-threaded")]
#[thread_local]
static TLS_DUMMY: u8 = 0;

#[wasm_bindgen(start)]
pub fn wasm_start() {
    #[cfg(feature = "wasm-threaded")]
    {
        // Ensure the TLS dummy is not optimized away.
        let _ = &TLS_DUMMY as *const u8;
    }
}

/// Placeholder API exported to JS. Both the threaded and single WASM variants
/// are built from this crate and must expose an identical surface.
#[wasm_bindgen]
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

#[wasm_bindgen]
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[wasm_bindgen]
pub struct AeroApi {
    version: String,
}

#[wasm_bindgen]
impl AeroApi {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn version(&self) -> String {
        self.version.clone()
    }
}
