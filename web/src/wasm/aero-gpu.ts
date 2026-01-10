// Temporary JS stub for the `aero-gpu` wasm-bindgen surface.
//
// Task dependency note:
// - Task 109 is expected to provide real wasm exports with this shape.
// - Until then, this stub lets the browser worker harness compile and provides
//   deterministic WebGPU/WebGL2 test pattern + screenshot paths for smoke testing.

import type { BackendKind, FrameTimingsReport, GpuAdapterInfo, GpuWorkerInitOptions } from '../ipc/gpu-messages';

import type { PresentationBackend } from '../../gpu/backend';
import { WebGL2Backend } from '../../gpu/webgl2_backend';
import { WebGPUBackend } from '../../gpu/webgpu_backend';

let backend: BackendKind | null = null;
let backendImpl: PresentationBackend | null = null;
let adapterInfo: GpuAdapterInfo | undefined;

let canvas: OffscreenCanvas | null = null;

let cssWidth = 0;
let cssHeight = 0;
let devicePixelRatio = 1;

let pixelWidth = 1;
let pixelHeight = 1;

function clampNonZero(n: number): number {
  if (!Number.isFinite(n)) return 1;
  return Math.max(1, Math.round(n));
}

function resizeInternal(width: number, height: number, dpr: number): void {
  cssWidth = width;
  cssHeight = height;
  devicePixelRatio = dpr || 1;

  pixelWidth = clampNonZero(width * devicePixelRatio);
  pixelHeight = clampNonZero(height * devicePixelRatio);

  if (canvas) {
    canvas.width = pixelWidth;
    canvas.height = pixelHeight;
  }
}

export default async function wasmInit(): Promise<void> {
  // Real wasm-bindgen init() will fetch/instantiate the wasm module.
}

export async function init_gpu(
  offscreenCanvas: OffscreenCanvas,
  width: number,
  height: number,
  dpr: number,
  options: GpuWorkerInitOptions = {},
): Promise<void> {
  canvas = offscreenCanvas;
  backendImpl = null;
  backend = null;
  adapterInfo = undefined;

  resizeInternal(width, height, dpr);

  const wantWebGpu = options.preferWebGpu === true && options.disableWebGpu !== true;
  const impl: PresentationBackend = wantWebGpu ? new WebGPUBackend() : new WebGL2Backend();
  await impl.init(offscreenCanvas, wantWebGpu ? { requiredFeatures: options.requiredFeatures as GPUFeatureName[] } : undefined);

  backendImpl = impl;
  backend = impl.getCapabilities().kind;

  if (backend === 'webgl2') {
    try {
      const gl = (impl as any).gl as WebGL2RenderingContext | null | undefined;
      if (gl) {
        const debugInfo = gl.getExtension('WEBGL_debug_renderer_info') as
          | {
              UNMASKED_VENDOR_WEBGL: number;
              UNMASKED_RENDERER_WEBGL: number;
            }
          | null;

        if (debugInfo) {
          adapterInfo = {
            vendor: String(gl.getParameter(debugInfo.UNMASKED_VENDOR_WEBGL)),
            renderer: String(gl.getParameter(debugInfo.UNMASKED_RENDERER_WEBGL)),
          };
        } else {
          // Best-effort fallback.
          adapterInfo = {
            vendor: String(gl.getParameter(gl.VENDOR)),
            renderer: String(gl.getParameter(gl.RENDERER)),
          };
        }
      }
    } catch {
      adapterInfo = undefined;
    }
  }
}

export function resize(width: number, height: number, dpr: number): void {
  if (!backendImpl || !canvas) return;
  resizeInternal(width, height, dpr);
}

export function backend_kind(): BackendKind {
  if (!backend) {
    throw new Error('GPU backend not initialized.');
  }
  return backend;
}

export function adapter_info(): GpuAdapterInfo | undefined {
  return adapterInfo;
}

export function capabilities(): unknown {
  if (!backendImpl || !backend) {
    return { initialized: false };
  }

  return {
    initialized: true,
    backend,
    backendCapabilities: backendImpl.getCapabilities(),
    cssSize: { width: cssWidth, height: cssHeight },
    devicePixelRatio,
    pixelSize: { width: pixelWidth, height: pixelHeight },
  };
}

export function present_test_pattern(): void | Promise<void> {
  if (!backendImpl) throw new Error('GPU backend not initialized.');

  const width = pixelWidth;
  const height = pixelHeight;

  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const rgba = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4;
      const left = x < halfW;
      const top = y < halfH;

      // Top-left origin:
      // - top-left: red
      // - top-right: green
      // - bottom-left: blue
      // - bottom-right: white
      let r = 0;
      let g = 0;
      let b = 0;
      if (top && left) {
        r = 255;
      } else if (top && !left) {
        g = 255;
      } else if (!top && left) {
        b = 255;
      } else {
        r = 255;
        g = 255;
        b = 255;
      }

      rgba[i + 0] = r;
      rgba[i + 1] = g;
      rgba[i + 2] = b;
      rgba[i + 3] = 255;
    }
  }

  backendImpl.uploadFrameRGBA(rgba, width, height);
  return backendImpl.present();
}

export async function request_screenshot(): Promise<Uint8Array> {
  if (!backendImpl) {
    throw new Error('GPU backend not initialized.');
  }

  const captured = await backendImpl.captureFrame();
  return new Uint8Array(captured.data.buffer, captured.data.byteOffset, captured.data.byteLength);
}

export function get_frame_timings(): FrameTimingsReport | null {
  // Not implemented in the JS stub. Real implementations should surface the
  // latest CPU/GPU timing report from the wasm runtime.
  return null;
}
