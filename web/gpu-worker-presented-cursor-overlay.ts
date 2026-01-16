import { createGpuWorker } from "./src/main/createGpuWorker";
import { srgbEncodeChannel } from "./src/gpu/test-card";
import { formatOneLineError } from "./src/text";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      backend?: string;
      error?: string;
      sampleNoCursor?: number[];
      expectedNoCursor?: number[];
      sample?: number[];
      expected?: number[];
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

function log(line: string) {
  const status = $("status");
  if (status) status.textContent += `${line}\n`;
}

type BackendParam = "auto" | "webgpu" | "webgl2_raw" | "webgl2_wgpu";

function getBackendParam(): BackendParam {
  const url = new URL(window.location.href);
  const value = (url.searchParams.get("backend") ?? "").trim();
  if (value === "webgpu" || value === "webgl2_raw" || value === "webgl2_wgpu") return value;
  return "auto";
}

function createBlueFrameRgba8(width: number, height: number): Uint8Array {
  const out = new Uint8Array(width * height * 4);
  for (let i = 0; i < out.length; i += 4) {
    out[i + 0] = 0;
    out[i + 1] = 0;
    out[i + 2] = 255;
    out[i + 3] = 255;
  }
  return out;
}

async function main() {
  const canvas = $("frame");
  if (!(canvas instanceof HTMLCanvasElement)) {
    renderError("Canvas element not found");
    return;
  }

  const width = 4;
  const height = 4;
  const devicePixelRatio = 1;

  canvas.width = width * devicePixelRatio;
  canvas.height = height * devicePixelRatio;
  canvas.style.width = `${width}px`;
  canvas.style.height = `${height}px`;

  try {
    const backend = getBackendParam();

    const gpu = createGpuWorker({
      canvas,
      width,
      height,
      devicePixelRatio,
      gpuOptions: {
        ...(backend !== "auto" ? { forceBackend: backend } : {}),
        presenter: {
          filter: "nearest",
          scaleMode: "stretch",
        },
      },
      onError: (msg) => {
        log(`gpu_error msg=${msg.message}${msg.code ? ` code=${msg.code}` : ""}`);
      },
      onEvents: (msg) => {
        for (const ev of msg.events) {
          log(`gpu_event ${ev.severity} ${ev.category}${ev.backend_kind ? ` (${ev.backend_kind})` : ""}: ${ev.message}`);
        }
      },
    });

    const ready = await gpu.ready;

    // Present solid blue as the base frame.
    gpu.presentRgba8(createBlueFrameRgba8(width, height));

    // Cursor image: 1x1 red @ 50% alpha.
    const cursorBytes = new Uint8Array([255, 0, 0, 128]);
    gpu.setCursorImageRgba8(1, 1, cursorBytes.buffer);
    gpu.setCursorState(true, 0, 0, 0, 0);

    const shotNoCursor = await gpu.requestPresentedScreenshot();
    const rgbaNoCursor = new Uint8Array(shotNoCursor.rgba8);
    const sampleNoCursor = [rgbaNoCursor[0]!, rgbaNoCursor[1]!, rgbaNoCursor[2]!, rgbaNoCursor[3]!];

    const a = 128 / 255;
    const expectedNoCursor = [0, 0, 255, 255];
    const expected = [srgbEncodeChannel(a), 0, srgbEncodeChannel(1 - a), 255];

    const shot = await gpu.requestPresentedScreenshot({ includeCursor: true });
    const rgba = new Uint8Array(shot.rgba8);
    const sample = [rgba[0]!, rgba[1]!, rgba[2]!, rgba[3]!];

    log(`backend=${ready.backendKind}`);
    log(`sample(no cursor)=${sampleNoCursor.join(",")}`);
    log(`expected(no cursor)=${expectedNoCursor.join(",")}`);
    log(`sample=${sample.join(",")}`);
    log(`expected≈${expected.join(",")} (tolerance ±2 on RGB)`);

    window.__aeroTest = {
      ready: true,
      backend: ready.backendKind,
      sampleNoCursor,
      expectedNoCursor,
      sample,
      expected,
    };
  } catch (err) {
    renderError(formatOneLineError(err, 512));
  }
}

void main();
