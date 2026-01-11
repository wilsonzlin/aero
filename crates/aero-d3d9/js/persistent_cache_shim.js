// WASM bindings for the browser-side persistent GPU cache.
//
// The canonical implementation lives in `web/gpu-cache/persistent_cache.ts` and
// installs itself as `globalThis.AeroPersistentGpuCache` when imported.
//
// This shim exists so `wasm-bindgen` can bundle a small JS module alongside the
// generated WASM package without reaching outside the crate directory.

function missingApiError() {
  return new Error(
    "AeroPersistentGpuCache is not installed on globalThis. " +
      "Import `web/gpu-cache/persistent_cache.ts` (or otherwise install " +
      "globalThis.AeroPersistentGpuCache) before using the D3D9 persistent shader cache.",
  );
}

/**
 * @param {Uint8Array} dxbc
 * @param {any} flags
 * @returns {Promise<string>}
 */
export async function computeShaderCacheKey(dxbc, flags) {
  const api = globalThis.AeroPersistentGpuCache;
  if (!api?.computeShaderCacheKey) {
    throw missingApiError();
  }
  return api.computeShaderCacheKey(dxbc, flags);
}

class MissingPersistentGpuCache {
  static async open() {
    throw missingApiError();
  }

  async getShader() {
    throw missingApiError();
  }

  async putShader() {
    throw missingApiError();
  }

  async deleteShader() {
    throw missingApiError();
  }
}

// Export a concrete class in all cases so the WASM module can instantiate even when
// the cache isn't available. When the host installs AeroPersistentGpuCache, replace
// this with the real implementation.
export let PersistentGpuCache = MissingPersistentGpuCache;
if (globalThis.AeroPersistentGpuCache?.PersistentGpuCache) {
  PersistentGpuCache = globalThis.AeroPersistentGpuCache.PersistentGpuCache;
}
