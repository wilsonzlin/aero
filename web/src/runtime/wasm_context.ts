import { initWasm, type WasmApi, type WasmVariant } from "./wasm_loader";

export type { WasmApi, WasmVariant };

type InitResult = { api: WasmApi; variant: WasmVariant };

let initPromise: Promise<InitResult> | undefined;

/**
 * Initialize the project's WASM module in whichever JS context we are running in
 * (main thread or DedicatedWorkerGlobalScope).
 *
 * This function must not reference `window`, since workers don't have it.
 */
export async function initWasmForContext(): Promise<InitResult> {
  if (!initPromise) {
    initPromise = initWasm()
      .then(({ api, variant }) => ({ api, variant }))
      .catch((err) => {
        // Allow retries if initialization fails (e.g. missing assets during dev).
        initPromise = undefined;
        throw err;
      });
  }

  return initPromise;
}
