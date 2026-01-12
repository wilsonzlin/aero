import type { BackendKind, FrameTimingsReport, GpuAdapterInfo, GpuWorkerInitOptions } from "../ipc/gpu-protocol";

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
