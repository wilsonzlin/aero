import { VgaPresenter } from "./src/display/vga_presenter";
import {
  FRAMEBUFFER_FORMAT_RGBA8888,
  HEADER_INDEX_CONFIG_COUNTER,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
  addHeaderI32,
  initFramebufferHeader,
  requiredFramebufferBytes,
  storeHeaderI32,
  wrapSharedFramebuffer,
} from "./src/display/framebuffer_protocol";
import { formatOneLineError } from "./src/text";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      transport?: "shared" | "copy";
      error?: string;
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

function createQuadrantPattern(width: number, height: number): Uint8Array {
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const out = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4;
      const left = x < halfW;
      const top = y < halfH;

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

  const cssWidth = 64;
  const cssHeight = 64;
  const devicePixelRatio = 1;

  canvas.width = cssWidth * devicePixelRatio;
  canvas.height = cssHeight * devicePixelRatio;
  canvas.style.width = `${cssWidth}px`;
  canvas.style.height = `${cssHeight}px`;

  const presenter = new VgaPresenter(canvas, { autoResizeToClient: false, scaleMode: "pixelated", integerScaling: true });
  presenter.start();

  try {
    const canUseShared =
      typeof SharedArrayBuffer !== "undefined" &&
      typeof Atomics !== "undefined" &&
      (typeof crossOriginIsolated !== "boolean" || crossOriginIsolated);

    if (canUseShared) {
      const sab = new SharedArrayBuffer(requiredFramebufferBytes(64, 64, 64 * 4));
      const shared = wrapSharedFramebuffer(sab, 0);
      initFramebufferHeader(shared.header, { width: 64, height: 64, strideBytes: 64 * 4, format: FRAMEBUFFER_FORMAT_RGBA8888 });
      presenter.setSharedFramebuffer(shared);

      // Frame 1: 64x64
      shared.pixelsU8.set(createQuadrantPattern(64, 64));
      addHeaderI32(shared.header, HEADER_INDEX_FRAME_COUNTER, 1);

      // Frame 2: 32x32 (exercise mode switch + integer scaling).
      storeHeaderI32(shared.header, HEADER_INDEX_WIDTH, 32);
      storeHeaderI32(shared.header, HEADER_INDEX_HEIGHT, 32);
      storeHeaderI32(shared.header, HEADER_INDEX_STRIDE_BYTES, 32 * 4);
      addHeaderI32(shared.header, HEADER_INDEX_CONFIG_COUNTER, 1);
      shared.pixelsU8.set(createQuadrantPattern(32, 32));
      addHeaderI32(shared.header, HEADER_INDEX_FRAME_COUNTER, 1);

      // Present synchronously to avoid flakiness in headless environments.
      presenter.presentLatestFrame();

      if (status) status.textContent = "transport=shared\n";

      window.__aeroTest = {
        ready: true,
        transport: "shared",
        samplePixels: async () => sampleCanvas(canvas),
      };
      return;
    }

    // Fallback: copy-frame path (no SharedArrayBuffer required).
    const frame64 = createQuadrantPattern(64, 64);
    presenter.pushCopyFrame({
      width: 64,
      height: 64,
      strideBytes: 64 * 4,
      format: FRAMEBUFFER_FORMAT_RGBA8888,
      frameCounter: 1,
      pixelsU8: frame64,
    });

    const frame32 = createQuadrantPattern(32, 32);
    presenter.pushCopyFrame({
      width: 32,
      height: 32,
      strideBytes: 32 * 4,
      format: FRAMEBUFFER_FORMAT_RGBA8888,
      frameCounter: 2,
      pixelsU8: frame32,
    });

    presenter.presentLatestFrame();

    if (status) status.textContent = "transport=copy\n";

    window.__aeroTest = {
      ready: true,
      transport: "copy",
      samplePixels: async () => sampleCanvas(canvas),
    };
  } catch (err) {
    renderError(formatOneLineError(err, 512));
  }
}

async function sampleCanvas(canvas: HTMLCanvasElement) {
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) throw new Error("Missing 2D context");

  const data = ctx.getImageData(0, 0, canvas.width, canvas.height).data;

  const sample = (x: number, y: number) => {
    const i = (y * canvas.width + x) * 4;
    return [data[i + 0], data[i + 1], data[i + 2], data[i + 3]];
  };

  return {
    width: canvas.width,
    height: canvas.height,
    topLeft: sample(8, 8),
    topRight: sample(canvas.width - 9, 8),
    bottomLeft: sample(8, canvas.height - 9),
    bottomRight: sample(canvas.width - 9, canvas.height - 9),
  };
}

void main();
