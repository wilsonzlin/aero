import { createGpuWorker } from "./src/main/createGpuWorker";
import { createGpuColorTestCardRgba8Linear, srgbEncodeChannel } from "./src/gpu/test-card";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      backend?: string;
      error?: string;
      hash?: string;
      expectedHash?: string;
      pass?: boolean;
      samplePixels?: () => Promise<{
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

function renderError(message: string) {
  const status = $("status");
  if (status) status.textContent = message;
  window.__aeroTest = { ready: true, error: message };
}

type BackendParam = "auto" | "webgpu" | "webgl2_raw" | "webgl2_wgpu";

function getBackendParam(): BackendParam {
  const url = new URL(window.location.href);
  const value = (url.searchParams.get("backend") ?? "").trim();
  if (value === "webgpu" || value === "webgl2_raw" || value === "webgl2_wgpu") return value;
  return "auto";
}

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as unknown as BufferSource);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function computeExpectedPresentedRgba8(cardLinear: Uint8Array, width: number, height: number): Uint8Array {
  const out = new Uint8Array(width * height * 4);
  for (let i = 0; i < width * height; i++) {
    const src = i * 4;
    const dst = src;
    out[dst + 0] = srgbEncodeChannel(cardLinear[src + 0]! / 255);
    out[dst + 1] = srgbEncodeChannel(cardLinear[src + 1]! / 255);
    out[dst + 2] = srgbEncodeChannel(cardLinear[src + 2]! / 255);
    // Presentation alpha policy: opaque.
    out[dst + 3] = 255;
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

  const width = 128;
  const height = 128;
  const devicePixelRatio = 1;

  canvas.width = width * devicePixelRatio;
  canvas.height = height * devicePixelRatio;
  canvas.style.width = `${width}px`;
  canvas.style.height = `${height}px`;

  try {
    const requestedBackend = getBackendParam();

    const gpu = createGpuWorker({
      canvas,
      width,
      height,
      devicePixelRatio,
      gpuOptions: {
        ...(requestedBackend !== "auto" ? { forceBackend: requestedBackend } : {}),
        presenter: {
          filter: "nearest",
          scaleMode: "stretch",
        },
      },
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
    });

    const ready = await gpu.ready;
    if (backendEl) backendEl.textContent = ready.backendKind;

    const card = createGpuColorTestCardRgba8Linear(width, height);
    const expected = computeExpectedPresentedRgba8(card, width, height);
    const expectedHash = await sha256Hex(expected);

    gpu.presentRgba8(card);
    const shot = await gpu.requestPresentedScreenshot();

    const actual = new Uint8Array(shot.rgba8);
    const hash = await sha256Hex(actual);

    const pass = shot.width === width && shot.height === height && hash === expectedHash;

    if (status) {
      status.textContent += `backend=${ready.backendKind}\n`;
      status.textContent += `hash=${hash}\n`;
      status.textContent += `expected=${expectedHash}\n`;
      status.textContent += pass ? "PASS\n" : "FAIL\n";
    }

    const sample = (x: number, y: number) => {
      const i = (y * shot.width + x) * 4;
      return [actual[i + 0], actual[i + 1], actual[i + 2], actual[i + 3]];
    };

    window.__aeroTest = {
      ready: true,
      backend: ready.backendKind,
      hash,
      expectedHash,
      pass,
      samplePixels: async () => ({
        width: shot.width,
        height: shot.height,
        topLeft: sample(0, 0),
        topRight: sample(shot.width - 1, 0),
        bottomLeft: sample(0, shot.height - 1),
        bottomRight: sample(shot.width - 1, shot.height - 1),
      }),
    };
  } catch (err) {
    renderError(err instanceof Error ? err.message : String(err));
  }
}

void main();

