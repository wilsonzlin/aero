import { initWasm } from "./wasm_loader";

export type WasmVariant = "single" | "threaded";

export type WasmApi = {
  version(): number;
  sum(a: number, b: number): number;
};

type InitResult = { api: WasmApi; variant: WasmVariant };

let initPromise: Promise<InitResult> | undefined;

function supportsWasmThreads(): boolean {
  // Threads require:
  // - cross-origin isolation (COOP/COEP) => crossOriginIsolated === true
  // - SharedArrayBuffer available
  // - Wasm shared memory support
  if ((globalThis as any).crossOriginIsolated !== true) return false;
  if (typeof SharedArrayBuffer === "undefined") return false;
  try {
    // Feature detect shared Wasm memory.
    // eslint-disable-next-line no-new
    new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    return true;
  } catch {
    return false;
  }
}

function selectVariantForContext(): WasmVariant {
  // "threaded" can be swapped to a real threads-enabled wasm build later.
  return supportsWasmThreads() ? "threaded" : "single";
}

async function instantiateWasm(url: URL): Promise<WebAssembly.WebAssemblyInstantiatedSource> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to fetch wasm (${response.status}): ${url.toString()}`);
  }

  const contentType = response.headers.get("Content-Type") ?? "";
  if (!contentType.includes("application/wasm")) {
    // Vite dev/preview should serve `.wasm` with `application/wasm`.
    // Keep running with a fallback, but surface the misconfiguration.
    console.warn(
      `WASM response has unexpected Content-Type "${contentType}" for ${url.toString()}`
    );
  }

  if (typeof WebAssembly.instantiateStreaming === "function") {
    try {
      return await WebAssembly.instantiateStreaming(response.clone(), {});
    } catch (err) {
      console.warn("WebAssembly.instantiateStreaming failed; falling back", err);
    }
  }

  const bytes = await response.arrayBuffer();
  return await WebAssembly.instantiate(bytes, {});
}

function toWasmApi(exports: WebAssembly.Exports): WasmApi {
  const exp = exports as unknown as Partial<WasmApi>;
  if (typeof exp.version !== "function" || typeof exp.sum !== "function") {
    throw new Error(
      'WASM exports missing expected functions: "version" and "sum"'
    );
  }
  return { version: exp.version, sum: exp.sum };
}

/**
 * Initialize the project's WASM module in whichever JS context we are running in
 * (main thread or DedicatedWorkerGlobalScope).
 *
 * This function must not reference `window`, since workers don't have it.
 */
export async function initWasmForContext(): Promise<InitResult> {
  if (!initPromise) {
    initPromise = (async () => {
      try {
        const { api, variant } = await initWasm();
        if (typeof api.version !== "function" || typeof api.sum !== "function") {
          throw new Error('WASM package missing expected exports: "version" and "sum"');
        }
        return { api, variant };
      } catch (err) {
        // Keep the legacy "embedded demo wasm" path as a fallback so the worker harness can
        // still run in a fresh checkout (before running `npm run wasm:build`).
        console.warn("wasm_loader init failed; falling back to embedded demo wasm", err);

        const variant = selectVariantForContext();
        const wasmUrl =
          variant === "threaded"
            ? new URL("../wasm/aero_demo_threaded.wasm", import.meta.url)
            : new URL("../wasm/aero_demo_single.wasm", import.meta.url);

        const { instance } = await instantiateWasm(wasmUrl);
        const api = toWasmApi(instance.exports);
        return { api, variant };
      }
    })();
  }

  return initPromise;
}
