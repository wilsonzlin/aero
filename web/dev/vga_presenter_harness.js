import { VgaPresenter } from "./vga_presenter.js";
import {
  FRAMEBUFFER_FORMAT_RGBA8888,
  HEADER_INDEX_CONFIG_COUNTER,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
  addHeaderI32,
  initFramebufferHeader,
  isSharedArrayBuffer,
  requiredFramebufferBytes,
  storeHeaderI32,
  wrapSharedFramebuffer,
} from "./framebuffer_protocol.js";

const $ = /** @type {(id: string) => HTMLElement} */ (id) => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`Missing element: ${id}`);
  return el;
};

const canvas = /** @type {HTMLCanvasElement} */ ($("screen"));
const transportEl = $("transport");
const modeEl = $("mode");
const frameEl = $("frame");
const scaleEl = $("scale");
const toggleScaleBtn = /** @type {HTMLButtonElement} */ ($("toggleScale"));

/** @type {"auto" | "pixelated" | "smooth"} */
let scaleMode = "auto";
scaleEl.textContent = scaleMode;

toggleScaleBtn.addEventListener("click", () => {
  scaleMode = scaleMode === "auto" ? "pixelated" : scaleMode === "pixelated" ? "smooth" : "auto";
  scaleEl.textContent = scaleMode;
  presenter.options.scaleMode = scaleMode;
});

const presenter = new VgaPresenter(canvas, {
  scaleMode,
  integerScaling: true,
  maxPresentHz: 60,
});
presenter.start();

const modes = [
  { width: 320, height: 200 },
  { width: 640, height: 480 },
  { width: 1024, height: 768 },
];

let modeIndex = 0;
let frameCounter = 0;

function currentMode() {
  return modes[modeIndex % modes.length];
}

function setMode(index) {
  modeIndex = index;
  const m = currentMode();
  modeEl.textContent = `${m.width}x${m.height}`;
}

setMode(0);

const canUseShared = typeof SharedArrayBuffer !== "undefined" && (typeof crossOriginIsolated !== "boolean" || crossOriginIsolated);

if (canUseShared) {
  const maxWidth = Math.max(...modes.map((m) => m.width));
  const maxHeight = Math.max(...modes.map((m) => m.height));
  const maxStride = maxWidth * 4;

  const sab = new SharedArrayBuffer(requiredFramebufferBytes(maxWidth, maxHeight, maxStride));
  const shared = wrapSharedFramebuffer(sab, 0);

  initFramebufferHeader(shared.header, {
    width: currentMode().width,
    height: currentMode().height,
    strideBytes: currentMode().width * 4,
    format: FRAMEBUFFER_FORMAT_RGBA8888,
  });

  presenter.setSharedFramebuffer(shared);
  transportEl.textContent = "SharedArrayBuffer";

  /** @type {Uint8ClampedArray} */
  let pixels = shared.pixelsU8Clamped;

  const writeFrame = () => {
    const { width, height } = currentMode();
    const strideBytes = width * 4;

    // Detect mode changes and update header.
    if (
      Atomics.load(shared.header, HEADER_INDEX_WIDTH) !== width ||
      Atomics.load(shared.header, HEADER_INDEX_HEIGHT) !== height ||
      Atomics.load(shared.header, HEADER_INDEX_STRIDE_BYTES) !== strideBytes
    ) {
      storeHeaderI32(shared.header, HEADER_INDEX_WIDTH, width);
      storeHeaderI32(shared.header, HEADER_INDEX_HEIGHT, height);
      storeHeaderI32(shared.header, HEADER_INDEX_STRIDE_BYTES, strideBytes);
      addHeaderI32(shared.header, HEADER_INDEX_CONFIG_COUNTER, 1);
    }

    const t = performance.now() * 0.001;
    const rowBytes = width * 4;
    for (let y = 0; y < height; y++) {
      const base = y * strideBytes;
      for (let x = 0; x < width; x++) {
        const i = base + x * 4;
        pixels[i + 0] = (x + t * 60) & 255;
        pixels[i + 1] = (y + t * 35) & 255;
        pixels[i + 2] = ((x ^ y) + t * 20) & 255;
        pixels[i + 3] = 255;
      }

      // Make a moving scanline to help visually confirm scaling and update rate.
      const scan = (Math.floor(t * 90) + y) % width;
      const j = base + scan * 4;
      pixels[j + 0] = 255;
      pixels[j + 1] = 255;
      pixels[j + 2] = 255;
      pixels[j + 3] = 255;

      // Simple gradient bar on the right edge to show stride/padding isn't required.
      if (rowBytes < strideBytes) {
        pixels.fill(0, base + rowBytes, base + strideBytes);
      }
    }

    frameCounter = addHeaderI32(shared.header, HEADER_INDEX_FRAME_COUNTER, 1) + 1;
    frameEl.textContent = String(frameCounter);
  };

  // Simulate a faster producer than the presenter (~60Hz) to validate frame dropping.
  setInterval(writeFrame, 1000 / 120);

  // Cycle display modes to validate dynamic resize handling.
  setInterval(() => setMode((modeIndex + 1) % modes.length), 2500);
} else {
  transportEl.textContent = "Copy (no SharedArrayBuffer)";

  /** @type {Uint8Array} */
  let pixels = new Uint8Array(currentMode().width * currentMode().height * 4);

  const ensurePixels = () => {
    const { width, height } = currentMode();
    const required = width * height * 4;
    if (pixels.byteLength !== required) {
      pixels = new Uint8Array(required);
    }
  };

  const writeFrame = () => {
    ensurePixels();
    const { width, height } = currentMode();
    const strideBytes = width * 4;
    const t = performance.now() * 0.001;

    for (let y = 0; y < height; y++) {
      const base = y * strideBytes;
      for (let x = 0; x < width; x++) {
        const i = base + x * 4;
        pixels[i + 0] = (x + t * 60) & 255;
        pixels[i + 1] = (y + t * 35) & 255;
        pixels[i + 2] = ((x ^ y) + t * 20) & 255;
        pixels[i + 3] = 255;
      }
    }

    frameCounter++;
    frameEl.textContent = String(frameCounter);
    presenter.pushCopyFrame({
      width,
      height,
      strideBytes,
      format: FRAMEBUFFER_FORMAT_RGBA8888,
      frameCounter,
      pixelsU8: pixels,
    });
  };

  setInterval(writeFrame, 1000 / 120);
  setInterval(() => setMode((modeIndex + 1) % modes.length), 2500);
}

// Sanity checks for the harness environment.
if (typeof SharedArrayBuffer !== "undefined") {
  // eslint-disable-next-line no-unused-expressions
  isSharedArrayBuffer(new SharedArrayBuffer(4));
}

