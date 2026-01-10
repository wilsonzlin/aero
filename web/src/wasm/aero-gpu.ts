// Temporary JS stub for the `aero-gpu` wasm-bindgen surface.
//
// Task dependency note:
// - Task 109 is expected to provide real wasm exports with this shape.
// - Until then, this stub lets the browser worker harness compile and provides
//   a deterministic WebGL2 test pattern + screenshot path for smoke testing.

import type { BackendKind, GpuAdapterInfo, GpuWorkerInitOptions } from '../ipc/gpu-messages';

type WebGl2 = WebGL2RenderingContext;

let backend: BackendKind | null = null;
let adapterInfo: GpuAdapterInfo | undefined;

let canvas: OffscreenCanvas | null = null;
let gl: WebGl2 | null = null;

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

  if (gl) {
    gl.viewport(0, 0, pixelWidth, pixelHeight);
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
  // Simulate a WebGPU attempt (the real implementation lives in wasm).
  if (options.preferWebGpu) {
    throw new Error('WebGPU init not implemented in JS stub (expected from wasm).');
  }

  canvas = offscreenCanvas;

  // Preserve drawing buffer so `readPixels` consistently captures after present.
  gl = canvas.getContext('webgl2', {
    alpha: false,
    antialias: false,
    depth: false,
    stencil: false,
    preserveDrawingBuffer: true,
  }) as WebGl2 | null;

  if (!gl) {
    backend = null;
    throw new Error('WebGL2 context unavailable.');
  }

  backend = 'webgl2';

  const debugInfo = gl.getExtension('WEBGL_debug_renderer_info') as
    | {
        UNMASKED_VENDOR_WEBGL: number;
        UNMASKED_RENDERER_WEBGL: number;
      }
    | null;
  if (debugInfo) {
    adapterInfo = {
      vendor: gl.getParameter(debugInfo.UNMASKED_VENDOR_WEBGL),
      renderer: gl.getParameter(debugInfo.UNMASKED_RENDERER_WEBGL),
    };
  } else {
    adapterInfo = undefined;
  }

  resizeInternal(width, height, dpr);
}

export function resize(width: number, height: number, dpr: number): void {
  if (!gl || !canvas) return;
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
  if (!gl || !backend) {
    return { initialized: false };
  }

  return {
    initialized: true,
    backend,
    cssSize: { width: cssWidth, height: cssHeight },
    devicePixelRatio,
    pixelSize: { width: pixelWidth, height: pixelHeight },
    gl: {
      version: gl.getParameter(gl.VERSION),
      shadingLanguageVersion: gl.getParameter(gl.SHADING_LANGUAGE_VERSION),
      vendor: gl.getParameter(gl.VENDOR),
      renderer: gl.getParameter(gl.RENDERER),
      maxTextureSize: gl.getParameter(gl.MAX_TEXTURE_SIZE),
      maxViewportDims: gl.getParameter(gl.MAX_VIEWPORT_DIMS),
      supportedExtensions: gl.getSupportedExtensions(),
    },
  };
}

export function present_test_pattern(): void {
  if (!gl) {
    throw new Error('GPU backend not initialized.');
  }

  gl.disable(gl.DITHER);
  gl.disable(gl.BLEND);

  gl.enable(gl.SCISSOR_TEST);

  const halfW = Math.floor(pixelWidth / 2);
  const halfH = Math.floor(pixelHeight / 2);

  // Note: WebGL scissor origin is bottom-left.
  gl.scissor(0, 0, halfW, halfH);
  gl.clearColor(1, 0, 0, 1);
  gl.clear(gl.COLOR_BUFFER_BIT);

  gl.scissor(halfW, 0, pixelWidth - halfW, halfH);
  gl.clearColor(0, 1, 0, 1);
  gl.clear(gl.COLOR_BUFFER_BIT);

  gl.scissor(0, halfH, halfW, pixelHeight - halfH);
  gl.clearColor(0, 0, 1, 1);
  gl.clear(gl.COLOR_BUFFER_BIT);

  gl.scissor(halfW, halfH, pixelWidth - halfW, pixelHeight - halfH);
  gl.clearColor(1, 1, 1, 1);
  gl.clear(gl.COLOR_BUFFER_BIT);

  gl.disable(gl.SCISSOR_TEST);
  gl.finish();
}

export function request_screenshot(): Uint8Array {
  if (!gl) {
    throw new Error('GPU backend not initialized.');
  }

  const pixels = new Uint8Array(pixelWidth * pixelHeight * 4);
  gl.readPixels(0, 0, pixelWidth, pixelHeight, gl.RGBA, gl.UNSIGNED_BYTE, pixels);

  // Convert to top-left origin for easier hashing/comparison on the main thread.
  const rowStride = pixelWidth * 4;
  const flipped = new Uint8Array(pixels.length);
  for (let y = 0; y < pixelHeight; y += 1) {
    const srcStart = (pixelHeight - 1 - y) * rowStride;
    const dstStart = y * rowStride;
    flipped.set(pixels.subarray(srcStart, srcStart + rowStride), dstStart);
  }
  return flipped;
}

