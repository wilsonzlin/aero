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

/**
 * Order-independent wrapper class used by wasm-bindgen bindings.
 *
 * Previously this module would snapshot `globalThis.AeroPersistentGpuCache.PersistentGpuCache`
 * at module evaluation time, which made import order significant. This wrapper resolves the
 * real implementation at call time and returns a stable JS class identity so Rust
 * `dyn_into::<JsPersistentGpuCache>()` continues to work regardless of host import order.
 */
export class PersistentGpuCache {
  /**
   * @param {any} inner
   */
  constructor(inner) {
    this._inner = inner;
  }

  static async open(...args) {
    const api = globalThis.AeroPersistentGpuCache;
    const impl = api?.PersistentGpuCache;
    if (!impl?.open) {
      throw missingApiError();
    }
    const inner = await impl.open(...args);
    return new PersistentGpuCache(inner);
  }

  /**
   * @param {string} key
   * @returns {Promise<any>}
   */
  async getShader(key) {
    if (!this._inner?.getShader) {
      throw missingApiError();
    }
    return this._inner.getShader(key);
  }

  /**
   * @param {string} key
   * @param {any} value
   * @returns {Promise<void>}
   */
  async putShader(key, value) {
    if (!this._inner?.putShader) {
      throw missingApiError();
    }
    return this._inner.putShader(key, value);
  }

  /**
   * @param {string} key
   * @returns {Promise<void>}
   */
  async deleteShader(key) {
    if (!this._inner?.deleteShader) {
      throw missingApiError();
    }
    return this._inner.deleteShader(key);
  }
}
