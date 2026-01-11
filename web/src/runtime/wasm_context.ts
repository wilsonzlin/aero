import { initWasm, type WasmApi, type WasmInitOptions, type WasmVariant } from "./wasm_loader";

export type { WasmApi, WasmVariant };

type InitResult = { api: WasmApi; variant: WasmVariant };

let initPromise: Promise<InitResult> | undefined;
const initPromiseByMemory = new WeakMap<WebAssembly.Memory, Promise<InitResult>>();

/**
 * Initialize the project's WASM module in whichever JS context we are running in
 * (main thread or DedicatedWorkerGlobalScope).
 *
 * This function must not reference `window`, since workers don't have it.
 */
export async function initWasmForContext(options: WasmInitOptions = {}): Promise<InitResult> {
  const memory = options.memory;
  if (memory) {
    const cached = initPromiseByMemory.get(memory);
    if (cached) return cached;

    const promise = initWasm(options)
      .then(({ api, variant }) => ({ api, variant }))
      .catch((err) => {
        // Allow retries if initialization fails (e.g. missing assets during dev).
        initPromiseByMemory.delete(memory);
        throw err;
      });
    initPromiseByMemory.set(memory, promise);
    return promise;
  }

  if (!initPromise) {
    initPromise = initWasm(options)
      .then(({ api, variant }) => ({ api, variant }))
      .catch((err) => {
        // Allow retries if initialization fails (e.g. missing assets during dev).
        initPromise = undefined;
        throw err;
      });
  }

  return initPromise;
}
