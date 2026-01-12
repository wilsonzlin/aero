import { createGpuWorker } from "./src/main/createGpuWorker";
import { fnv1a32Hex } from "./src/utils/fnv1a";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      backend?: string;
      fallback?: {
        from: string;
        to: string;
        reason: string;
      };
      error?: string;
      hash?: string;
      expectedHash?: string;
      pass?: boolean;
      lastStats?: unknown;
      lastWasmStats?: unknown;
      samplePixels?: () => Promise<{
        backend: string;
        width: number;
        height: number;
        topLeft: number[];
        topRight: number[];
        bottomLeft: number[];
        bottomRight: number[];
      }>;
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function getBoolParam(name: string): boolean {
  const url = new URL(window.location.href);
  const value = url.searchParams.get(name);
  if (value === null) return false;
  return value !== "0" && value !== "false";
}

function getBackendParam(): "auto" | "webgpu" | "webgl2" {
  const url = new URL(window.location.href);
  const value = url.searchParams.get("backend");
  if (value === "webgpu" || value === "webgl2") return value;
  return "auto";
}

function renderError(message: string) {
  const status = $("status");
  if (status) status.textContent = message;
  window.__aeroTest = { ready: true, error: message };
}

function safeJsonStringify(value: unknown): string {
  try {
    return JSON.stringify(value, (_key, v) => (typeof v === "bigint" ? v.toString() : v));
  } catch {
    try {
      return String(value);
    } catch {
      return "<unprintable>";
    }
  }
}

function createExpectedTestPattern(width: number, height: number): Uint8Array {
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const out = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const left = x < halfW;
      const top = y < halfH;

      // Top-left origin:
      // - top-left: red
      // - top-right: green
      // - bottom-left: blue
      // - bottom-right: white
      if (top && left) {
        out[i + 0] = 255;
        out[i + 1] = 0;
        out[i + 2] = 0;
        out[i + 3] = 255;
      } else if (top && !left) {
        out[i + 0] = 0;
        out[i + 1] = 255;
        out[i + 2] = 0;
        out[i + 3] = 255;
      } else if (!top && left) {
        out[i + 0] = 0;
        out[i + 1] = 0;
        out[i + 2] = 255;
        out[i + 3] = 255;
      } else {
        out[i + 0] = 255;
        out[i + 1] = 255;
        out[i + 2] = 255;
        out[i + 3] = 255;
      }
    }
  }

  return out;
}

async function main() {
  const canvas = $("frame");
  if (!(canvas instanceof HTMLCanvasElement)) {
    renderError("Canvas element not found");
    return;
  }

  const status = $("status");
  const backendEl = $("backend");

  const cssWidth = 64;
  const cssHeight = 64;
  const devicePixelRatio = 1;

  canvas.width = cssWidth * devicePixelRatio;
  canvas.height = cssHeight * devicePixelRatio;
  canvas.style.width = `${cssWidth}px`;
  canvas.style.height = `${cssHeight}px`;

  try {
    let statsLinesWritten = 0;
    const requestedBackend = getBackendParam();
    const disableWebGpu = getBoolParam("disableWebGpu");
    // In practice, WebGPU-in-worker can be flaky in headless Chromium unless
    // launched with the dedicated WebGPU project flags. Prefer WebGL2 for the
    // default smoke test, but still allow forcing WebGPU (or testing fallback)
    // via query params.
    const preferWebGpu = requestedBackend === "webgpu" || disableWebGpu;

    const gpu = createGpuWorker({
      canvas,
      width: cssWidth,
      height: cssHeight,
      devicePixelRatio,
      gpuOptions: { preferWebGpu, disableWebGpu },
      onError: (msg) => {
        if (!status) return;
        status.textContent += `gpu_error msg=${msg.message}${msg.code ? ` code=${msg.code}` : ""}\n`;
      },
      onEvents: (msg) => {
        if (!status) return;
        for (const ev of msg.events) {
          status.textContent += `gpu_event ${ev.severity} ${ev.category}: ${ev.message}\n`;
        }
      },
      onStats: (msg) => {
        // Print a small preview of WASM telemetry so the page can be used as a
        // diagnostics wiring check (particularly for the wgpu-backed WebGL2 presenter).
        window.__aeroTest = { ...(window.__aeroTest ?? {}), lastStats: msg, lastWasmStats: msg.wasm };
        if (!status) return;
        if (!msg.wasm) return;
        if (statsLinesWritten >= 1) return;
        statsLinesWritten += 1;
        const preview = safeJsonStringify(msg.wasm);
        status.textContent += `gpu_stats wasm=${preview.slice(0, 400)}${preview.length > 400 ? "…" : ""}\n`;
      },
    });

    const ready = await gpu.ready;
    if (backendEl) backendEl.textContent = ready.backendKind;
    if (ready.fallback && status) {
      status.textContent += `fallback ${ready.fallback.from} -> ${ready.fallback.to}: ${ready.fallback.reason}\n`;
    }

    gpu.presentTestPattern();
    const screenshot = await gpu.requestScreenshot();

    const rgba8 = new Uint8Array(screenshot.rgba8);
    const expected = createExpectedTestPattern(screenshot.width, screenshot.height);

    const hash = fnv1a32Hex(rgba8);
    const expectedHash = fnv1a32Hex(expected);
    const pass = hash === expectedHash;

    if (status) {
      status.textContent += `hash=${hash} expected=${expectedHash} ${pass ? "PASS" : "FAIL"}\n`;
    }

    const sample = (x: number, y: number) => {
      const i = (y * screenshot.width + x) * 4;
      return [rgba8[i + 0], rgba8[i + 1], rgba8[i + 2], rgba8[i + 3]];
    };

    window.__aeroTest = {
      ready: true,
      backend: ready.backendKind,
      fallback: ready.fallback,
      hash,
      expectedHash,
      pass,
      lastStats: window.__aeroTest?.lastStats,
      lastWasmStats: window.__aeroTest?.lastWasmStats,
      samplePixels: async () => ({
        backend: ready.backendKind,
        width: screenshot.width,
        height: screenshot.height,
        topLeft: sample(8, 8),
        topRight: sample(screenshot.width - 9, 8),
        bottomLeft: sample(8, screenshot.height - 9),
        bottomRight: sample(screenshot.width - 9, screenshot.height - 9),
      }),
    };
  } catch (err) {
    renderError(err instanceof Error ? err.message : String(err));
  }
}

void main();
