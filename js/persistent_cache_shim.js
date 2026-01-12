// Root entrypoint for the D3D9 persistent shader cache JS shim.
//
// `aero-d3d9`'s WASM bindings import this module via an absolute specifier:
// `#[wasm_bindgen(module = "/js/persistent_cache_shim.js")]`.
//
// Vite resolves `/js/...` relative to the repo root, so we keep the runtime path
// stable by providing this thin re-export layer.

export * from "../crates/aero-d3d9/js/persistent_cache_shim.js";
export {
  computeShaderCacheKey,
  PersistentGpuCache,
} from "../crates/aero-d3d9/js/persistent_cache_shim.js";
