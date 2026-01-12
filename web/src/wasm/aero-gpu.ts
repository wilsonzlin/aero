import type { BackendKind, FrameTimingsReport, GpuAdapterInfo, GpuWorkerInitOptions } from "../ipc/gpu-protocol";

// Ensure the persistent GPU cache API is installed on `globalThis` before any WASM code tries to
// open it (used for D3D9 DXBC->WGSL shader translation caching).
import "../../gpu-cache/persistent_cache.ts";

type WasmVariant = "threaded" | "single";

interface ThreadSupport {
  supported: boolean;
  reason: string;
}

function detectThreadSupport(): ThreadSupport {
  // `crossOriginIsolated` is required for SharedArrayBuffer on the web.
  if (!(globalThis as any).crossOriginIsolated) {
    return {
      supported: false,
      reason: "crossOriginIsolated is false (missing COOP/COEP headers); SharedArrayBuffer is unavailable",
    };
  }

  if (typeof SharedArrayBuffer === "undefined") {
    return { supported: false, reason: "SharedArrayBuffer is undefined (not supported or not enabled)" };
  }

  if (typeof Atomics === "undefined") {
    return { supported: false, reason: "Atomics is undefined (WASM threads are not supported)" };
  }

  if (typeof WebAssembly === "undefined" || typeof WebAssembly.Memory === "undefined") {
    return { supported: false, reason: "WebAssembly.Memory is unavailable in this environment" };
  }

  try {
    // eslint-disable-next-line no-new
    new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return { supported: false, reason: `Shared WebAssembly.Memory is not supported: ${message}` };
  }

  return { supported: true, reason: "crossOriginIsolated + SharedArrayBuffer + Atomics + shared WebAssembly.Memory" };
}

type RawAeroGpuWasmModule = any;

// `wasm-pack` outputs into `web/src/wasm/pkg-single-gpu` and `web/src/wasm/pkg-threaded-gpu`.
//
// These directories are generated (see `web/scripts/build_wasm.mjs`) and are not
// necessarily present in a fresh checkout. Use `import.meta.glob` so:
//  - Vite builds don't fail when the generated output is missing.
//  - When the output *is* present, it is bundled as usual.
const wasmImporters = import.meta.glob("./pkg-*/aero_gpu_wasm.js");

let loaded: RawAeroGpuWasmModule | null = null;

function requireLoaded(): RawAeroGpuWasmModule {
  if (!loaded) {
    throw new Error("aero-gpu wasm module not initialized. Call the default init() export first.");
  }
  return loaded;
}

async function loadVariant(variant: WasmVariant): Promise<RawAeroGpuWasmModule> {
  const releasePath = variant === "threaded" ? "./pkg-threaded-gpu/aero_gpu_wasm.js" : "./pkg-single-gpu/aero_gpu_wasm.js";
  const devPath =
    variant === "threaded" ? "./pkg-threaded-gpu-dev/aero_gpu_wasm.js" : "./pkg-single-gpu-dev/aero_gpu_wasm.js";

  const importer = wasmImporters[releasePath] ?? wasmImporters[devPath];
  if (!importer) {
    if (import.meta.env.DEV) {
      // When running the Vite dev server *before* `web/src/wasm/pkg-*/` exists (e.g. in local E2E
      // workflows using `AERO_PLAYWRIGHT_REUSE_SERVER=1`), `import.meta.glob()` can miss newly
      // generated wasm-pack output until the server is restarted.
      //
      // In dev mode only, fall back to a runtime `import()` so developers can rebuild WASM without
      // restarting Vite.
      const tryDynamicImport = async (path: string): Promise<RawAeroGpuWasmModule | null> => {
        try {
          return (await import(/* @vite-ignore */ path)) as RawAeroGpuWasmModule;
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
        "Missing aero-gpu WASM package.",
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

  const mod = (await importer()) as RawAeroGpuWasmModule;
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

export async function init_gpu(
  offscreenCanvas: OffscreenCanvas,
  width: number,
  height: number,
  dpr: number,
  options: GpuWorkerInitOptions = {},
): Promise<void> {
  const mod = requireLoaded();
  await mod.init_gpu(offscreenCanvas, width, height, dpr, options);
}

export function resize(width: number, height: number, dpr: number, outputWidth?: number, outputHeight?: number): void {
  const mod = requireLoaded();
  // `outputWidth/outputHeight` are optional; when omitted we pass 0 so the wasm
  // side can keep the existing override configured at init time.
  mod.resize(width, height, dpr, outputWidth ?? 0, outputHeight ?? 0);
}

export function backend_kind(): BackendKind {
  const mod = requireLoaded();
  return mod.backend_kind() as BackendKind;
}

export function adapter_info(): GpuAdapterInfo | undefined {
  const mod = requireLoaded();
  return mod.adapter_info() as GpuAdapterInfo | undefined;
}

export function capabilities(): unknown {
  const mod = requireLoaded();
  return mod.capabilities();
}

export function present_test_pattern(): void {
  const mod = requireLoaded();
  mod.present_test_pattern();
}

export function present_rgba8888(frame: Uint8Array, strideBytes: number): void {
  const mod = requireLoaded();
  mod.present_rgba8888(frame, strideBytes);
}

export async function request_screenshot(): Promise<Uint8Array> {
  const mod = requireLoaded();
  return (await mod.request_screenshot()) as Uint8Array;
}

export function get_frame_timings(): FrameTimingsReport | null {
  const mod = requireLoaded();
  if (typeof mod.get_frame_timings !== "function") return null;
  return mod.get_frame_timings() as FrameTimingsReport | null;
}

export function destroy_gpu(): void {
  const mod = requireLoaded();
  mod.destroy_gpu();
}

/**
 * Register a view of the guest RAM backing store for AeroGPU submissions.
 *
 * Note: on PC/Q35, guest physical RAM is non-contiguous once the configured guest RAM exceeds the
 * PCIe ECAM base (0xB000_0000): the "high" portion is remapped above 4 GiB, leaving an ECAM/PCI
 * hole below 4 GiB. AeroGPU uses guest physical addresses (GPAs), so the WASM module translates
 * GPAs back into this flat backing store before indexing.
 */
export function set_guest_memory(guestU8: Uint8Array): void {
  const mod = requireLoaded();
  mod.set_guest_memory(guestU8);
}

export function clear_guest_memory(): void {
  const mod = requireLoaded();
  mod.clear_guest_memory();
}

export function has_guest_memory(): boolean {
  const mod = requireLoaded();
  return !!mod.has_guest_memory?.();
}

/**
 * Debug helper: read bytes from guest RAM at the given guest physical address.
 *
 * `gpa` is a guest physical address (subject to the same hole/high-RAM remap translation as
 * allocations/submissions), not a direct offset into the backing `Uint8Array`.
 */
export function read_guest_memory(gpa: bigint, len: number): Uint8Array {
  const mod = requireLoaded();
  return mod.read_guest_memory(gpa, len) as Uint8Array;
}

export type SubmitAerogpuResult = {
  completedFence: bigint;
  presentCount?: bigint;
};

export function submit_aerogpu(cmdStream: Uint8Array, signalFence: bigint, allocTable?: Uint8Array): SubmitAerogpuResult {
  const mod = requireLoaded();
  return mod.submit_aerogpu(cmdStream, signalFence, allocTable) as SubmitAerogpuResult;
}

export function has_submit_aerogpu_d3d9(): boolean {
  const mod = requireLoaded();
  return typeof mod.submit_aerogpu_d3d9 === "function";
}

export async function init_aerogpu_d3d9(
  offscreenCanvas?: OffscreenCanvas | null,
  options: GpuWorkerInitOptions = {},
): Promise<void> {
  const mod = requireLoaded();
  // wasm-bindgen uses `undefined` for `Option<T>`.
  await mod.init_aerogpu_d3d9(offscreenCanvas ?? undefined, options);
}

export async function submit_aerogpu_d3d9(
  cmdStream: Uint8Array,
  signalFence: bigint,
  contextId: number,
  allocTable?: Uint8Array,
): Promise<SubmitAerogpuResult> {
  const mod = requireLoaded();
  if (typeof mod.submit_aerogpu_d3d9 !== "function") {
    throw new Error("aero-gpu wasm export submit_aerogpu_d3d9 is missing (outdated bundle?)");
  }
  return (await mod.submit_aerogpu_d3d9(cmdStream, signalFence, contextId, allocTable)) as SubmitAerogpuResult;
}

export type ScreenshotInfo = {
  width: number;
  height: number;
  rgba8: ArrayBuffer;
  origin?: "top-left";
};

export async function request_screenshot_info(): Promise<ScreenshotInfo> {
  const mod = requireLoaded();
  return (await mod.request_screenshot_info()) as ScreenshotInfo;
}

// -----------------------------------------------------------------------------
// Optional diagnostics exports (best-effort; may be missing on older bundles)
// -----------------------------------------------------------------------------

function wrapNonThrowing(result: unknown): unknown | undefined {
  // Some wasm-bindgen exports are async and return a promise. Ensure telemetry
  // wrappers never throw by converting rejections into `undefined`.
  if (result && typeof (result as { then?: unknown }).then === "function") {
    return (result as PromiseLike<unknown>).then(
      (value) => value,
      () => undefined,
    );
  }
  return result;
}

export function get_gpu_stats(): unknown | undefined {
  try {
    const mod = requireLoaded();
    const fn =
      typeof mod.get_gpu_stats === "function"
        ? (mod.get_gpu_stats as () => unknown)
        : typeof mod.getGpuStats === "function"
          ? (mod.getGpuStats as () => unknown)
          : typeof mod.get_stats === "function"
            ? (mod.get_stats as () => unknown)
            : typeof mod.getStats === "function"
              ? (mod.getStats as () => unknown)
              : null;
    if (!fn) return undefined;
    return wrapNonThrowing(fn());
  } catch {
    return undefined;
  }
}

export function drain_gpu_events(): unknown | undefined {
  try {
    const mod = requireLoaded();
    const fn =
      typeof mod.drain_gpu_events === "function"
        ? (mod.drain_gpu_events as () => unknown)
        : typeof mod.drain_gpu_error_events === "function"
          ? (mod.drain_gpu_error_events as () => unknown)
          : typeof mod.take_gpu_events === "function"
            ? (mod.take_gpu_events as () => unknown)
            : typeof mod.take_gpu_error_events === "function"
              ? (mod.take_gpu_error_events as () => unknown)
              : typeof mod.drainGpuEvents === "function"
                ? (mod.drainGpuEvents as () => unknown)
                : null;
    if (!fn) return undefined;
    return wrapNonThrowing(fn());
  } catch {
    return undefined;
  }
}
