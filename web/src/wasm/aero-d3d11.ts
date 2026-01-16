// Ensure the persistent GPU cache API is installed on `globalThis` before any WASM code tries to
// open it (used for D3D11 DXBC->WGSL shader translation caching).
import "../../gpu-cache/persistent_cache.ts";

type WasmVariant = "threaded" | "single";

interface ThreadSupport {
  supported: boolean;
}

function detectThreadSupport(): ThreadSupport {
  const coi = (globalThis as unknown as { crossOriginIsolated?: unknown }).crossOriginIsolated;
  if (coi !== true) return { supported: false };
  if (typeof SharedArrayBuffer === "undefined") return { supported: false };
  if (typeof Atomics === "undefined") return { supported: false };
  if (typeof WebAssembly === "undefined" || typeof WebAssembly.Memory !== "function") return { supported: false };
  try {
    // eslint-disable-next-line no-new
    new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    return { supported: true };
  } catch {
    return { supported: false };
  }
}

type RawAeroD3d11WasmModule = any;

const wasmImporters = import.meta.glob("./pkg-*/aero_d3d11_wasm.js");
const IS_DEV = (import.meta as { env?: { DEV?: boolean } }).env?.DEV === true;

let loaded: RawAeroD3d11WasmModule | null = null;

function requireLoaded(): RawAeroD3d11WasmModule {
  if (!loaded) {
    throw new Error("aero-d3d11 wasm module not initialized. Call the default init() export first.");
  }
  return loaded;
}

async function loadVariant(variant: WasmVariant): Promise<RawAeroD3d11WasmModule> {
  const releasePath = variant === "threaded" ? "./pkg-threaded-d3d11/aero_d3d11_wasm.js" : "./pkg-single-d3d11/aero_d3d11_wasm.js";
  const devPath = variant === "threaded" ? "./pkg-threaded-d3d11-dev/aero_d3d11_wasm.js" : "./pkg-single-d3d11-dev/aero_d3d11_wasm.js";

  const importer = wasmImporters[releasePath] ?? wasmImporters[devPath];
  if (!importer) {
    if (IS_DEV) {
      const tryDynamicImport = async (path: string): Promise<RawAeroD3d11WasmModule | null> => {
        try {
          return (await import(/* @vite-ignore */ path)) as RawAeroD3d11WasmModule;
        } catch {
          return null;
        }
      };
      const mod = (await tryDynamicImport(releasePath)) ?? (await tryDynamicImport(devPath));
      if (mod) {
        await mod.default();
        return mod;
      }
    }
    throw new Error(
      [
        "Missing aero-d3d11 WASM package.",
        "",
        "Build it with:",
        "  cd web",
        `  npm run wasm:build:${variant}`,
        "",
        "Or build both variants:",
        "  npm run wasm:build",
      ].join("\n"),
    );
  }

  const mod = (await importer()) as RawAeroD3d11WasmModule;
  await mod.default();
  return mod;
}

export default async function init(): Promise<void> {
  if (loaded) return;

  const threadSupport = detectThreadSupport();
  if (threadSupport.supported) {
    try {
      loaded = await loadVariant("threaded");
      return;
    } catch {
      // Fall back to single if the threaded build isn't present or fails to init.
    }
  }

  loaded = await loadVariant("single");
}

export async function run_d3d11_shader_cache_demo(capsHash?: string | null): Promise<unknown> {
  const mod = requireLoaded();
  return (await mod.run_d3d11_shader_cache_demo(capsHash ?? null)) as unknown;
}
