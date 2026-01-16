import { createGpuWorker } from "./src/main/createGpuWorker";
import { createGpuColorTestCardRgba8Linear, srgbEncodeChannel } from "./src/gpu/test-card";
import { formatOneLineError } from "./src/text";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      backend?: string;
      error?: string;
      hash?: string;
      samplePixels?: () => Promise<{
        width: number;
        height: number;
        topLeft: number[];
        topRight: number[];
        bottomLeft: number[];
        bottomRight: number[];
        midGray: number[];
        midGrayExpected: number[];
        alphaZero: number[];
        alphaZeroExpected: number[];
      }>;
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function renderError(message: string) {
  const status = $("status");
  if (status) {
    // Keep any GPU worker diagnostics already printed (events/errors) and append the final
    // top-level error message. This is a test/debug page; retaining the full context makes
    // Playwright failure snapshots much more actionable.
    status.textContent += status.textContent ? `\n${message}` : message;
  }
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

function computeExpectedPresentedPixel(cardLinear: Uint8Array, width: number, x: number, y: number): number[] {
  const i = (y * width + x) * 4;
  return [
    srgbEncodeChannel(cardLinear[i + 0]! / 255),
    srgbEncodeChannel(cardLinear[i + 1]! / 255),
    srgbEncodeChannel(cardLinear[i + 2]! / 255),
    255, // Presentation alpha policy: opaque.
  ];
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
          let details = "";
          try {
            if (ev.details != null) details = ` details=${JSON.stringify(ev.details)}`;
          } catch {
            // Ignore: best-effort formatting only.
          }
          status.textContent += `gpu_event ${ev.severity} ${ev.category}${ev.backend_kind ? ` (${ev.backend_kind})` : ""}: ${ev.message}${details}\n`;
        }
      },
    });

    const ready = await gpu.ready;
    if (backendEl) backendEl.textContent = ready.backendKind;

    const card = createGpuColorTestCardRgba8Linear(width, height);

    gpu.presentRgba8(card);
    const shot = await gpu.requestPresentedScreenshot();

    const actual = new Uint8Array(shot.rgba8);
    const hash = await sha256Hex(actual);

    if (status) {
      status.textContent += `backend=${ready.backendKind}\n`;
      status.textContent += `hash=${hash}\n`;
    }

    const sample = (x: number, y: number) => {
      const i = (y * shot.width + x) * 4;
      return [actual[i + 0], actual[i + 1], actual[i + 2], actual[i + 3]];
    };

    // Key sample points used by Playwright to validate gamma + alpha policy without relying on a
    // full-frame CPU reference hash (which can be sensitive to GPU/driver rounding differences).
    const midGrayX = Math.floor(width / 4);
    const midGrayY = Math.floor(height / 2);
    const alphaZeroX = Math.max(0, width - 2);
    const alphaZeroY = 0;

    window.__aeroTest = {
      ready: true,
      backend: ready.backendKind,
      hash,
      samplePixels: async () => ({
        width: shot.width,
        height: shot.height,
        topLeft: sample(0, 0),
        topRight: sample(shot.width - 1, 0),
        bottomLeft: sample(0, shot.height - 1),
        bottomRight: sample(shot.width - 1, shot.height - 1),
        midGray: sample(midGrayX, midGrayY),
        midGrayExpected: computeExpectedPresentedPixel(card, width, midGrayX, midGrayY),
        alphaZero: sample(alphaZeroX, alphaZeroY),
        alphaZeroExpected: computeExpectedPresentedPixel(card, width, alphaZeroX, alphaZeroY),
      }),
    };
  } catch (err) {
    renderError(formatOneLineError(err, 512));
  }
}

void main();
